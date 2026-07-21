//! Page discovery: walk `pages/`, parse frontmatter, and validate page config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omniapp_core::MarkdownDocument;
use omniapp_schema::{
    Filter, Model, Order, Problem, View, path_placeholders, valid_output_template,
};

use crate::SiteError;
use crate::context::Permalink;

/// A discovered site page with its parsed frontmatter and routing intent.
#[derive(Debug)]
pub(crate) struct Page {
    /// Path relative to the site directory, e.g. `pages/about.md`.
    pub source_rel: PathBuf,
    /// Template name used for the page body (also drives autoescaping).
    pub template_name: String,
    pub is_markdown: bool,
    pub body: String,
    pub title: Option<String>,
    pub layout: Option<String>,
    /// All frontmatter keys, exposed to templates as `page.<key>`.
    pub frontmatter: serde_yaml::Mapping,
    pub kind: PageKind,
}

/// How a page maps onto output routes.
#[derive(Debug)]
pub(crate) enum PageKind {
    /// A single page at a fixed URL.
    Static { url: String, output: PathBuf },
    /// The `404.html` page: rendered but not routed.
    NotFound,
    /// One output page per record.
    Generator(Generator),
}

#[derive(Debug)]
pub(crate) struct Generator {
    pub permalink: Permalink,
    pub source: GeneratorSource,
}

#[derive(Debug)]
pub(crate) enum GeneratorSource {
    Model {
        model: String,
        filters: Vec<Filter>,
        order: Vec<Order>,
    },
    View {
        view: String,
        model: String,
    },
}

/// Discover and validate every page under `<site_dir>/pages`.
pub(crate) fn discover(
    site_dir: &Path,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
) -> Result<Vec<Page>, SiteError> {
    let pages_dir = site_dir.join("pages");
    let mut files = Vec::new();
    collect_files(&pages_dir, &mut files)?;
    files.sort();

    let mut pages = Vec::new();
    let mut problems = Vec::new();
    for file in files {
        let rel_under_pages = file
            .strip_prefix(&pages_dir)
            .map_err(|_| SiteError::Invalid("page escaped the pages directory".into()))?
            .to_path_buf();
        let source_rel = Path::new("pages").join(&rel_under_pages);
        let contents = std::fs::read_to_string(&file).map_err(|source| SiteError::Io {
            path: file.display().to_string(),
            source,
        })?;
        let document = MarkdownDocument::parse(&contents, &file)?;
        match build_page(&rel_under_pages, source_rel, document, models, views) {
            Ok(page) => pages.push(page),
            Err(mut page_problems) => problems.append(&mut page_problems),
        }
    }
    if !problems.is_empty() {
        return Err(SiteError::config("pages", &problems));
    }
    Ok(pages)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), SiteError> {
    if !dir.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|source| SiteError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SiteError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn build_page(
    rel_under_pages: &Path,
    source_rel: PathBuf,
    document: MarkdownDocument,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
) -> Result<Page, Vec<Problem>> {
    let location = source_rel.display().to_string();
    let frontmatter = document.frontmatter;
    let is_markdown = source_rel
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    let template_name = source_rel.to_string_lossy().replace('\\', "/");

    let title = string_key(&frontmatter, "title");
    let layout = string_key(&frontmatter, "layout");
    let permalink = string_key(&frontmatter, "permalink");
    let model = string_key(&frontmatter, "model");
    let view = string_key(&frontmatter, "view");

    let mut problems = Vec::new();
    let kind = classify(
        rel_under_pages,
        &location,
        permalink.as_deref(),
        model.as_deref(),
        view.as_deref(),
        &frontmatter,
        models,
        views,
        &mut problems,
    );
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Page {
        source_rel,
        template_name,
        is_markdown,
        body: document.body,
        title,
        layout,
        frontmatter,
        kind: kind.expect("kind is present when there are no problems"),
    })
}

#[allow(clippy::too_many_arguments)]
fn classify(
    rel_under_pages: &Path,
    location: &str,
    permalink: Option<&str>,
    model: Option<&str>,
    view: Option<&str>,
    frontmatter: &serde_yaml::Mapping,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
    problems: &mut Vec<Problem>,
) -> Option<PageKind> {
    let is_generator = model.is_some() || view.is_some();
    if is_generator {
        return classify_generator(
            location,
            permalink,
            model,
            view,
            frontmatter,
            models,
            views,
            problems,
        );
    }

    // Static page. Reject generator-only keys if present without model/view.
    if frontmatter.contains_key("filters") || frontmatter.contains_key("order") {
        problems.push(Problem::new(
            location,
            "filters and order require a model generator page",
        ));
    }

    if rel_under_pages == Path::new("404.html") {
        return Some(PageKind::NotFound);
    }

    if let Some(permalink) = permalink {
        let parsed = Permalink::parse(permalink);
        if !valid_output_template(parsed.template()) {
            problems.push(Problem::new(
                location,
                format!("permalink {permalink:?} is not a safe path template"),
            ));
            return None;
        }
        if !path_placeholders(parsed.template()).is_empty() {
            problems.push(Problem::new(
                location,
                "only generator pages may use permalink placeholders",
            ));
            return None;
        }
        match parsed.render(&BTreeMap::new()) {
            Ok((url, output)) => Some(PageKind::Static { url, output }),
            Err(error) => {
                problems.push(Problem::new(location, error.to_string()));
                None
            }
        }
    } else {
        let (url, output) = derive_route(rel_under_pages);
        Some(PageKind::Static { url, output })
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_generator(
    location: &str,
    permalink: Option<&str>,
    model: Option<&str>,
    view: Option<&str>,
    frontmatter: &serde_yaml::Mapping,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
    problems: &mut Vec<Problem>,
) -> Option<PageKind> {
    if model.is_some() && view.is_some() {
        problems.push(Problem::new(location, "set either model or view, not both"));
        return None;
    }
    let Some(permalink_raw) = permalink else {
        problems.push(Problem::new(
            location,
            "generator pages require a permalink",
        ));
        return None;
    };
    let permalink = Permalink::parse(permalink_raw);
    if !valid_output_template(permalink.template()) {
        problems.push(Problem::new(
            location,
            format!("permalink {permalink_raw:?} is not a safe path template"),
        ));
        return None;
    }

    let source = if let Some(view_name) = view {
        if frontmatter.contains_key("filters") || frontmatter.contains_key("order") {
            problems.push(Problem::new(
                location,
                "view generators cannot also set filters or order",
            ));
        }
        let Some(view) = views.get(view_name) else {
            problems.push(Problem::new(
                location,
                format!("unknown view {view_name:?}"),
            ));
            return None;
        };
        check_placeholders(
            location,
            permalink.template(),
            &view.model,
            models,
            problems,
        );
        GeneratorSource::View {
            view: view_name.to_owned(),
            model: view.model.clone(),
        }
    } else {
        let model_name = model.expect("generator has model when it has no view");
        if !models.contains_key(model_name) {
            problems.push(Problem::new(
                location,
                format!("unknown model {model_name:?}"),
            ));
            return None;
        }
        check_placeholders(location, permalink.template(), model_name, models, problems);
        let filters = deserialize_key(frontmatter, "filters", location, problems);
        let order = deserialize_key(frontmatter, "order", location, problems);
        GeneratorSource::Model {
            model: model_name.to_owned(),
            filters,
            order,
        }
    };

    if !problems.is_empty() {
        return None;
    }
    Some(PageKind::Generator(Generator { permalink, source }))
}

fn check_placeholders(
    location: &str,
    permalink: &str,
    model_name: &str,
    models: &BTreeMap<String, Model>,
    problems: &mut Vec<Problem>,
) {
    let Some(model) = models.get(model_name) else {
        return;
    };
    let placeholders = path_placeholders(permalink);
    if placeholders.is_empty() {
        problems.push(Problem::new(
            location,
            "generator permalink must vary per record with a {field} placeholder",
        ));
    }
    for placeholder in placeholders {
        if !model.fields.contains_key(&placeholder) {
            problems.push(Problem::new(
                location,
                format!("permalink references unknown field {placeholder:?} on model {model_name}"),
            ));
        }
    }
}

fn deserialize_key<T: serde::de::DeserializeOwned>(
    frontmatter: &serde_yaml::Mapping,
    key: &str,
    location: &str,
    problems: &mut Vec<Problem>,
) -> Vec<T> {
    let Some(value) = frontmatter.get(serde_yaml::Value::String(key.to_owned())) else {
        return Vec::new();
    };
    match serde_yaml::from_value::<Vec<T>>(value.clone()) {
        Ok(items) => items,
        Err(error) => {
            problems.push(Problem::new(location, format!("invalid {key}: {error}")));
            Vec::new()
        }
    }
}

/// Map a page path to its pretty URL and output file path. Only `.html` and
/// `.md` pages get pretty directory URLs; any other extension is a raw page
/// (`sitemap.xml`, `llms.txt`, `robots.txt`, …) whose URL and output keep the
/// path under `pages/` verbatim — still Jinja-rendered, never layout-derived.
fn derive_route(rel_under_pages: &Path) -> (String, PathBuf) {
    let pretty = rel_under_pages
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("html")
                || extension.eq_ignore_ascii_case("htm")
                || extension.eq_ignore_ascii_case("md")
        });
    if !pretty {
        let joined = rel_under_pages
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        return (format!("/{joined}"), PathBuf::from(joined));
    }
    let mut stem = rel_under_pages.to_path_buf();
    stem.set_extension("");
    let mut base: Vec<String> = stem
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if base.last().map(String::as_str) == Some("index") {
        base.pop();
    }
    if base.is_empty() {
        ("/".to_owned(), PathBuf::from("index.html"))
    } else {
        let joined = base.join("/");
        (
            format!("/{joined}/"),
            PathBuf::from(format!("{joined}/index.html")),
        )
    }
}

fn string_key(frontmatter: &serde_yaml::Mapping, key: &str) -> Option<String> {
    frontmatter
        .get(serde_yaml::Value::String(key.to_owned()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_owned)
}
