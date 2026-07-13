//! The build pipeline: render every route into a staging directory, copy
//! assets, and atomically swap it into place only when there were no errors.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use omniapp_core::Workspace;
use omniapp_schema::{FieldType, is_safe_relative};

use crate::env::build_environment;
use crate::render::Renderer;
use crate::routes::{RenderSpec, Route};
use crate::{BuildOptions, BuildProblem, BuildReport, LoadedSite, SiteError, not_found_route};

/// Build one or all sites (see [`BuildOptions::site`]) into `_site/<name>`
/// directories (or `options.out_dir` for a single site). Validation and the
/// record sync run once; each site then renders into a sibling staging
/// directory that only replaces the existing output when that site's build is
/// clean. A site whose pages fail to render keeps its previous output and
/// reports errors, without stopping the other sites.
pub fn build(
    workspace: &Workspace,
    options: &BuildOptions,
) -> Result<Vec<(String, BuildReport)>, SiteError> {
    // One sync feeds validation and every site's rendering.
    let synced = workspace.sync_cache()?;
    let report = workspace.validate_synced(&synced);
    if !report.is_valid() {
        let message = report
            .diagnostics
            .iter()
            .map(|diagnostic| format!("  {}: {}", diagnostic.location, diagnostic.message))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(SiteError::Validation(message));
    }

    let names = match &options.site {
        Some(name) => vec![name.clone()],
        None => workspace.site_names()?,
    };
    if names.is_empty() {
        return Err(SiteError::MissingSite {
            path: workspace.sites_dir(),
        });
    }
    let out_root = options
        .out_dir
        .clone()
        .unwrap_or_else(|| workspace.root().join("_site"));

    let mut reports = Vec::new();
    for name in names {
        let out_dir = if options.site.is_some() && options.out_dir.is_some() {
            out_root.clone()
        } else {
            out_root.join(&name)
        };
        let report = build_site(
            workspace,
            &synced.loaded,
            &synced.records,
            &name,
            &out_dir,
            options,
        )?;
        reports.push((name, report));
    }
    Ok(reports)
}

fn build_site(
    workspace: &Workspace,
    loaded: &omniapp_core::LoadedWorkspace,
    records: &[omniapp_core::Record],
    name: &str,
    out_dir: &Path,
    options: &BuildOptions,
) -> Result<BuildReport, SiteError> {
    let site_dir = workspace.site_dir(name);
    if !site_dir.exists() {
        return Err(SiteError::MissingSite { path: site_dir });
    }

    let mut site = LoadedSite::load_with(workspace, name, loaded, records.to_vec())?;
    if let Some(base_url) = &options.base_url {
        site.settings.url = Some(base_url.clone());
    }

    let out_dir = out_dir.to_path_buf();
    let staging = staging_dir(&out_dir);
    if staging.exists() {
        remove_dir(&staging)?;
    }
    create_dir(&staging)?;

    let time = Utc::now().to_rfc3339();
    let mut env = build_environment(&site_dir, options.strict);
    let renderer = Renderer::new(&site.data, &site.pages, &site.settings, &site.views, &time);

    let mut errors = site.load_problems.clone();
    let mut pages = 0;
    let mut record_pages = 0;

    let mut render = |route: &Route, errors: &mut Vec<BuildProblem>| match renderer
        .render_route(&mut env, route)
    {
        Ok(html) => write_output(&staging, &route.output, &html).map(|()| true),
        Err(error) => {
            errors.push(problem_for(route, &site.pages, &error));
            Ok(false)
        }
    };

    for route in &site.routes {
        if render(route, &mut errors)? {
            match route.render {
                RenderSpec::Page(_) => pages += 1,
                RenderSpec::Record(..) => record_pages += 1,
            }
        }
    }
    if let Some(index) = site.not_found
        && render(&not_found_route(index), &mut errors)?
    {
        pages += 1;
    }

    let site_assets = copy_assets(&site_dir, &staging)?;
    let record_assets = copy_record_assets(&site, &staging)?;

    let report = BuildReport {
        out_dir: out_dir.clone(),
        pages,
        record_pages,
        site_assets,
        record_assets,
        errors,
    };

    if !report.errors.is_empty() {
        remove_dir(&staging)?;
        return Ok(report);
    }

    if out_dir.exists() {
        remove_dir(&out_dir)?;
    }
    if let Some(parent) = out_dir.parent() {
        create_dir(parent)?;
    }
    fs::rename(&staging, &out_dir).map_err(|source| io_error(&staging, source))?;
    Ok(report)
}

fn problem_for(route: &Route, pages: &[crate::pages::Page], error: &SiteError) -> BuildProblem {
    let location = match &route.render {
        RenderSpec::Page(index) => pages[*index].source_rel.display().to_string(),
        RenderSpec::Record(index, record) => {
            format!("{} ({})", pages[*index].source_rel.display(), record.key)
        }
    };
    let message = match error {
        SiteError::Template(inner) => format!("{inner:#}"),
        other => other.to_string(),
    };
    BuildProblem { location, message }
}

fn write_output(staging: &Path, relative: &Path, html: &str) -> Result<(), SiteError> {
    let path = staging.join(relative);
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    fs::write(&path, html).map_err(|source| io_error(&path, source))
}

fn copy_assets(site_dir: &Path, staging: &Path) -> Result<usize, SiteError> {
    let source = site_dir.join("assets");
    if !source.exists() {
        return Ok(0);
    }
    copy_tree(&source, &staging.join("assets"))
}

fn copy_tree(source: &Path, dest: &Path) -> Result<usize, SiteError> {
    let mut count = 0;
    let entries = fs::read_dir(source).map_err(|error| io_error(source, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(source, error))?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            count += copy_tree(&path, &target)?;
        } else if path.is_file() {
            if let Some(parent) = target.parent() {
                create_dir(parent)?;
            }
            fs::copy(&path, &target).map_err(|error| io_error(&path, error))?;
            count += 1;
        }
    }
    Ok(count)
}

fn copy_record_assets(site: &LoadedSite, staging: &Path) -> Result<usize, SiteError> {
    let canonical_root = site
        .root
        .canonicalize()
        .map_err(|error| io_error(&site.root, error))?;
    let mut copied = BTreeSet::new();
    for model in site.data.models().values() {
        let asset_fields = model
            .fields
            .iter()
            .filter(|(_, field)| field.field_type == FieldType::Asset)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if asset_fields.is_empty() {
            continue;
        }
        for record in site.data.model_records(&model.name) {
            for field in &asset_fields {
                let Some(value) = record.values.get(field).and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                if copied.contains(value) {
                    continue;
                }
                if copy_one_asset(&site.root, &canonical_root, staging, value)? {
                    copied.insert(value.to_owned());
                }
            }
        }
    }
    Ok(copied.len())
}

fn copy_one_asset(
    root: &Path,
    canonical_root: &Path,
    staging: &Path,
    value: &str,
) -> Result<bool, SiteError> {
    if !is_safe_relative(value) || Path::new(value).starts_with(".omniapp") {
        return Ok(false);
    }
    let source = root.join(value);
    if !source.is_file() {
        return Ok(false);
    }
    let canonical = source
        .canonicalize()
        .map_err(|error| io_error(&source, error))?;
    if !canonical.starts_with(canonical_root) {
        return Ok(false);
    }
    let dest = staging.join("files").join(value);
    if let Some(parent) = dest.parent() {
        create_dir(parent)?;
    }
    fs::copy(&source, &dest).map_err(|error| io_error(&source, error))?;
    Ok(true)
}

fn staging_dir(out_dir: &Path) -> PathBuf {
    match out_dir.file_name() {
        Some(name) => out_dir.with_file_name(format!("{}.new", name.to_string_lossy())),
        None => PathBuf::from(format!("{}.new", out_dir.display())),
    }
}

fn create_dir(path: &Path) -> Result<(), SiteError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn remove_dir(path: &Path) -> Result<(), SiteError> {
    fs::remove_dir_all(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: std::io::Error) -> SiteError {
    SiteError::Io {
        path: path.display().to_string(),
        source,
    }
}
