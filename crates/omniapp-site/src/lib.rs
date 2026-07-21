//! Jekyll-like static site generation over a project's records.
//!
//! Owns site config/page discovery, the template environment, record context
//! resolution, the route table, and the build pipeline. Must not own HTTP
//! serving or record parsing/writing (those live in omniapp-web and
//! omniapp-core respectively).

mod build;
mod config;
mod context;
mod env;
mod pages;
mod render;
mod routes;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use omniapp_schema::{Problem, View};
use serde::Serialize;
use thiserror::Error;

use crate::config::SiteSettings;
use crate::context::{Permalink, SiteData};
use crate::env::build_environment;
use crate::pages::{GeneratorSource, Page, PageKind};
use crate::render::Renderer;
use crate::routes::{RenderSpec, Route, build_routes};

pub use build::build;
pub use env::render_markdown;

/// Options controlling a site build.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Build only this site; `None` builds every site in `.omniapp/sites`.
    pub site: Option<String>,
    /// Output root. With `site` set it is that site's exact output
    /// directory; otherwise each site builds into `<out_dir>/<name>`.
    /// Defaults to `<project root>/_site`.
    pub out_dir: Option<PathBuf>,
    /// Overrides `site.url` for this build (single-site builds).
    pub base_url: Option<String>,
    /// Treat undefined template lookups as errors.
    pub strict: bool,
}

/// A non-fatal problem encountered while building (e.g. one page failed to
/// render). The build still produces a report; it just refuses to publish.
#[derive(Debug, Clone, Serialize)]
pub struct BuildProblem {
    pub location: String,
    pub message: String,
}

/// Summary of a build run.
#[derive(Debug, Clone, Serialize)]
pub struct BuildReport {
    pub out_dir: PathBuf,
    pub pages: usize,
    pub record_pages: usize,
    pub site_assets: usize,
    pub record_assets: usize,
    pub errors: Vec<BuildProblem>,
}

/// The outcome of resolving a URL against the loaded site.
#[derive(Debug)]
pub enum Resolution {
    /// A rendered page.
    Html(String),
    /// A rendered non-HTML page (`sitemap.xml`, `llms.txt`, …) and the
    /// content type it should be served with.
    Raw {
        content_type: &'static str,
        body: String,
    },
    /// The URL should redirect to a canonical (slash-terminated) form.
    Redirect(String),
    /// No page matched; `html` is the rendered 404 page if one exists.
    NotFound { html: Option<String> },
}

/// The serve content type for a non-HTML output file, by extension. `None`
/// means the route is an ordinary HTML page.
fn raw_content_type(output: &Path) -> Option<&'static str> {
    let extension = output.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => None,
        "xml" => Some("application/xml; charset=utf-8"),
        "json" | "webmanifest" => Some("application/json; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "css" => Some("text/css; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        _ => Some("text/plain; charset=utf-8"),
    }
}

#[derive(Debug, Error)]
pub enum SiteError {
    #[error(transparent)]
    Workspace(#[from] omniapp_core::WorkspaceError),
    #[error("no site sources: {} does not exist", .path.display())]
    MissingSite { path: PathBuf },
    #[error("project is not valid; fix these errors first:\n{0}")]
    Validation(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Template(#[from] minijinja::Error),
    #[error("URL collision at {url}: produced by both {first} and {second}")]
    Collision {
        url: String,
        first: String,
        second: String,
    },
    #[error("route {url} uses a reserved prefix (from {origin})")]
    Reserved { url: String, origin: String },
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

impl SiteError {
    /// Fold a list of config [`Problem`]s into a single readable error.
    pub(crate) fn config(scope: &str, problems: &[Problem]) -> Self {
        let joined = problems
            .iter()
            .map(|problem| format!("  {}: {}", problem.location, problem.message))
            .collect::<Vec<_>>()
            .join("\n");
        SiteError::Invalid(format!("{scope} configuration is invalid:\n{joined}"))
    }
}

/// One site loaded from `.omniapp/sites/<name>`, ready to build or resolve
/// URLs against.
pub struct LoadedSite {
    name: String,
    site_dir: PathBuf,
    root: PathBuf,
    has_site: bool,
    settings: SiteSettings,
    data: Arc<SiteData>,
    pages: Vec<Page>,
    views: BTreeMap<String, View>,
    routes: Vec<Route>,
    not_found: Option<usize>,
    load_problems: Vec<BuildProblem>,
}

impl LoadedSite {
    /// Load one named site's sources. Returns `Ok` with
    /// [`has_site`](Self::has_site) `== false` when the site directory is
    /// absent, so callers can handle missing sites without special-casing.
    pub fn load(workspace: &omniapp_core::Workspace, name: &str) -> Result<Self, SiteError> {
        if !workspace.site_dir(name).exists() {
            // Nothing to render; skip the record sync entirely.
            let loaded = workspace.load()?;
            return Self::load_with(workspace, name, &loaded, Vec::new());
        }
        let synced = workspace.sync_cache()?;
        let records = synced.records;
        Self::load_with(workspace, name, &synced.loaded, records)
    }

    /// Load a named site against already-loaded definitions and records —
    /// long-running callers (the web server) pass their cached snapshot
    /// instead of paying a filesystem scan per invalidation.
    pub fn load_with(
        workspace: &omniapp_core::Workspace,
        name: &str,
        loaded: &omniapp_core::LoadedWorkspace,
        records: Vec<omniapp_core::Record>,
    ) -> Result<Self, SiteError> {
        let site_dir = workspace.site_dir(name);
        let root = workspace.root().to_path_buf();
        if !site_dir.exists() {
            return Ok(Self {
                name: name.to_owned(),
                site_dir,
                root,
                has_site: false,
                settings: SiteSettings {
                    title: String::new(),
                    description: None,
                    url: None,
                    params: BTreeMap::new(),
                },
                data: SiteData::new(BTreeMap::new(), Vec::new(), BTreeMap::new()),
                pages: Vec::new(),
                views: BTreeMap::new(),
                routes: Vec::new(),
                not_found: None,
                load_problems: Vec::new(),
            });
        }

        let settings = SiteSettings::load(&site_dir, &loaded.config.name)?;
        let pages = pages::discover(&site_dir, &loaded.models, &loaded.views)?;
        let name = name.to_owned();

        let mut permalinks: BTreeMap<String, Permalink> = BTreeMap::new();
        for page in &pages {
            if let PageKind::Generator(generator) = &page.kind {
                let model = match &generator.source {
                    GeneratorSource::Model { model, .. } | GeneratorSource::View { model, .. } => {
                        model
                    }
                };
                permalinks
                    .entry(model.clone())
                    .or_insert_with(|| generator.permalink.clone());
            }
        }

        let data = SiteData::new(loaded.models.clone(), records, permalinks);
        let table = build_routes(&pages, &loaded.views, &data)?;

        Ok(Self {
            name,
            site_dir,
            root,
            has_site: true,
            settings,
            data,
            pages,
            views: loaded.views.clone(),
            routes: table.routes,
            not_found: table.not_found,
            load_problems: table.problems,
        })
    }

    #[must_use]
    pub fn has_site(&self) -> bool {
        self.has_site
    }

    /// The site's name (its directory under `.omniapp/sites`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Resolve a URL path to a page. `/assets` and `/files` are served
    /// elsewhere and are not considered here.
    pub fn resolve(&self, url_path: &str) -> Result<Resolution, SiteError> {
        if !self.has_site {
            return Ok(Resolution::NotFound { html: None });
        }
        let normalized = normalize_url(url_path);
        if let Some(route) = self.routes.iter().find(|route| route.url == normalized) {
            let body = self.render_route(route)?;
            return Ok(match raw_content_type(&route.output) {
                Some(content_type) => Resolution::Raw { content_type, body },
                None => Resolution::Html(body),
            });
        }
        if !normalized.ends_with('/') {
            let slashed = format!("{normalized}/");
            if self.routes.iter().any(|route| route.url == slashed) {
                return Ok(Resolution::Redirect(slashed));
            }
        }
        let html = match self.not_found {
            Some(index) => Some(self.render_route(&not_found_route(index))?),
            None => None,
        };
        Ok(Resolution::NotFound { html })
    }

    fn render_route(&self, route: &Route) -> Result<String, SiteError> {
        let time = Utc::now().to_rfc3339();
        let mut env = build_environment(&self.site_dir, false);
        let renderer = Renderer::new(&self.data, &self.pages, &self.settings, &self.views, &time);
        renderer.render_route(&mut env, route)
    }
}

/// The synthetic route used to render `pages/404.html`.
fn not_found_route(page_index: usize) -> Route {
    Route {
        url: "/404.html".to_owned(),
        output: PathBuf::from("404.html"),
        render: RenderSpec::Page(page_index),
    }
}

fn normalize_url(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path.is_empty() {
        "/".to_owned()
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

/// Render a small, self-contained HTML error page for the dev server.
#[must_use]
pub fn render_error_page(error: &SiteError) -> String {
    let detail = match error {
        SiteError::Template(inner) => format!("{inner:#}"),
        other => other.to_string(),
    };
    let escaped = html_escape(&detail);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Site error</title>\
<style>body{{font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;\
background:#1e1e1e;color:#eee;margin:0;padding:2rem}}\
h1{{font-family:system-ui,sans-serif;font-size:1.25rem;color:#ff8080}}\
pre{{background:#2a2a2a;border:1px solid #444;border-radius:6px;padding:1rem;\
overflow:auto;white-space:pre-wrap;word-break:break-word}}</style></head>\
<body><h1>Site error</h1><pre>{escaped}</pre></body></html>"
    )
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
