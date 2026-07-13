use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use omniapp_core::Workspace;
use tracing_subscriber::EnvFilter;

mod commands;

const DEFAULT_MODEL: &str = r#"# Model fields may be stored in a path, a shared YAML file, Markdown, or an asset.
version: 1
name: Book
label: Books
description: A collection of books stored as readable folders.
storage:
  kind: directory
  path: books/{slug}
fields:
  slug:
    type: string
    label: Slug
    required: true
    source:
      kind: path
      variable: slug
    validation:
      pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$"
  title:
    type: string
    label: Title
    required: true
    source:
      kind: yaml
      file: book.yml
      key: title
  author:
    type: string
    label: Author
    source:
      kind: yaml
      file: book.yml
      key: author
  status:
    type: enum
    label: Status
    default: planned
    source:
      kind: yaml
      file: book.yml
      key: status
    validation:
      choices: [planned, reading, complete]
  published_on:
    type: date
    label: Published
    source:
      kind: yaml
      file: book.yml
      key: published_on
  notes:
    type: text
    label: Notes
    source:
      kind: markdown
      file: notes.md
  cover:
    type: asset
    label: Cover
    source:
      kind: asset
      file: cover.jpg
outputs:
  publication: build/{slug}
"#;

const DEFAULT_VIEW: &str = r"version: 1
name: library
label: Library
model: Book
type: table
fields: [title, author, status, published_on]
query:
  order:
    - field: title
      direction: asc
  page_size: 50
actions: []
";

const DEFAULT_LAYOUT: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{% block title %}{{ site.title }}{% endblock %}</title>
  <link rel="stylesheet" href="/assets/main.css">
</head>
<body>
  <header class="site-header">
    <a class="site-title" href="/">{{ site.title }}</a>
  </header>
  <main>
    {% block main %}{% endblock %}
  </main>
  <footer class="site-footer">
    <p>{{ site.description }}</p>
  </footer>
</body>
</html>
"#;

const DEFAULT_INDEX_PAGE: &str = r#"{% extends "layouts/base.html" %}
{% block main %}
  <h1>{{ site.title }}</h1>
  <ul class="record-list">
    {% for book in records.Book %}
      <li>
        <a href="{{ book.url }}">{{ book.title }}</a>
        {% if book.author %}<span class="byline">by {{ book.author }}</span>{% endif %}
        <span class="status">{{ book.status }}</span>
      </li>
    {% else %}
      <li class="empty">No books yet.</li>
    {% endfor %}
  </ul>
{% endblock %}
"#;

const DEFAULT_RECORD_PAGE: &str = r#"---
model: Book
permalink: books/{slug}/
---
{% extends "layouts/base.html" %}
{% block title %}{{ record.title }} · {{ site.title }}{% endblock %}
{% block main %}
  <h1>{{ record.title }}</h1>
  {% if record.author %}<p class="byline">by {{ record.author }}</p>{% endif %}
  {% if record.cover %}<img class="cover" src="{{ record.cover | asset_url }}" alt="{{ record.title }}">{% endif %}
  {{ record.notes | markdown }}
{% endblock %}
"#;

const DEFAULT_SITE_CSS: &str = r":root { color-scheme: light }
body {
  margin: 0 auto;
  max-width: 42rem;
  padding: 0 1.25rem 4rem;
  color: #1d2321;
  background: #fdfdfc;
  font: 17px/1.65 ui-serif, Georgia, serif;
}
a { color: #245c47 }
h1 { font-size: 2rem; letter-spacing: -0.02em; line-height: 1.2 }
.site-header { padding: 2.2rem 0 2.6rem }
.site-title {
  color: inherit;
  text-decoration: none;
  font-weight: 700;
  font-size: 1.05rem;
  letter-spacing: 0.01em;
}
.byline { color: #68736e }
.status {
  font: 12px/1.6 ui-sans-serif, system-ui, sans-serif;
  color: #245c47;
  background: #dcebe4;
  border-radius: 99px;
  padding: 1px 9px;
  margin-left: 6px;
  vertical-align: 2px;
}
.record-list { list-style: none; padding: 0 }
.record-list li { padding: 0.45rem 0; border-bottom: 1px solid #e8ebe8 }
.cover { max-width: 100%; border-radius: 8px }
.site-footer { margin-top: 4rem; color: #68736e; font-size: 0.85rem }
";

const AGENTS_SECTION_START: &str = "<!-- omniapp:agents:start -->";
const AGENTS_SECTION_END: &str = "<!-- omniapp:agents:end -->";

#[derive(Debug, Parser)]
#[command(
    name = "omniapp",
    version,
    about = "A local-first platform for structured data"
)]
struct Cli {
    /// Emit machine-readable JSON instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a new project.
    Init {
        /// Project directory (defaults to the current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Display name written to config.yml.
        #[arg(long)]
        name: Option<String>,
    },
    /// Validate schemas, records, and references.
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Re-read every record from disk and rebuild the cache instead of
        /// trusting file fingerprints.
        #[arg(long)]
        full: bool,
    },
    /// Render the public site(s) to static directories under _site/.
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Build only this site (a directory name under .omniapp/sites).
        #[arg(long)]
        site: Option<String>,
        /// Output directory for a single site (requires --site).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Override the configured site.url for this build (requires --site).
        #[arg(long)]
        base_url: Option<String>,
        /// Treat undefined template variables as errors.
        #[arg(long)]
        strict: bool,
    },
    /// Serve every site (one port each) plus the admin application.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// First port to try. Each site takes the next available port, then
        /// the admin.
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Do not open the default browser.
        #[arg(long)]
        no_open: bool,
        /// Do not serve the admin application.
        #[arg(long)]
        no_admin: bool,
    },
    /// List records for a model.
    List {
        model: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 1)]
        page: usize,
        #[arg(long, default_value_t = 50)]
        page_size: usize,
    },
    /// Get one record by key, path, id, or slug.
    Get {
        model: String,
        key: String,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Create a record.
    Create {
        model: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Set a field. JSON values are decoded; other values are strings.
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        sets: Vec<String>,
        /// Read a JSON object from a file, or use `-` for stdin.
        #[arg(long, short)]
        input: Option<PathBuf>,
    },
    /// Update an existing record.
    Update {
        model: String,
        key: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Set a field. Use `null` to remove an optional value.
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        sets: Vec<String>,
        /// Read a JSON object from a file, or use `-` for stdin.
        #[arg(long, short)]
        input: Option<PathBuf>,
    },
    /// Delete a record after checking references and nested records.
    Delete {
        model: String,
        key: String,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Run a saved declarative view query.
    Query {
        view: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Override the view's configured page size.
        #[arg(long)]
        page_size: Option<usize>,
    },
    /// Search all indexed text fields using SQLite FTS5 syntax.
    Search {
        query: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Resolve outbound relationships and inbound backreferences.
    Relationships {
        model: String,
        key: String,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Resolve named generated-output paths for a record.
    Outputs {
        model: String,
        key: String,
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("omniapp=info")),
        )
        .with_target(false)
        .compact()
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path, name } => initialize(&path, name.as_deref(), cli.json),
        Command::Validate { path, full } => validate(&path, full, cli.json),
        Command::Build {
            path,
            site,
            out,
            base_url,
            strict,
        } => build(&path, site, out, base_url, strict, cli.json),
        Command::Serve {
            path,
            port,
            no_open,
            no_admin,
        } => serve(path, port, no_open, no_admin, cli.json).await,
        Command::List {
            model,
            path,
            page,
            page_size,
        } => commands::list(&path, &model, page, page_size, cli.json),
        Command::Get { model, key, path } => commands::get(&path, &model, &key, cli.json),
        Command::Create {
            model,
            path,
            sets,
            input,
        } => commands::create(&path, &model, &sets, input.as_deref(), cli.json),
        Command::Update {
            model,
            key,
            path,
            sets,
            input,
        } => commands::update(&path, &model, &key, &sets, input.as_deref(), cli.json),
        Command::Delete { model, key, path } => commands::delete(&path, &model, &key, cli.json),
        Command::Query {
            view,
            path,
            page,
            page_size,
        } => commands::query(&path, &view, page, page_size, cli.json),
        Command::Search { query, path, limit } => commands::search(&path, &query, limit, cli.json),
        Command::Relationships { model, key, path } => {
            commands::relationships(&path, &model, &key, cli.json)
        }
        Command::Outputs { model, key, path } => commands::outputs(&path, &model, &key, cli.json),
    }
}

fn initialize(path: &Path, requested_name: Option<&str>, json: bool) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("could not create {}", path.display()))?;
    let metadata = path.join(".omniapp");
    if metadata.exists() {
        bail!(
            "{} already exists; refusing to overwrite this project",
            metadata.display()
        );
    }
    fs::create_dir_all(metadata.join("models"))?;
    fs::create_dir_all(metadata.join("views"))?;
    fs::create_dir_all(metadata.join("scripts"))?;
    let inferred_name = path
        .canonicalize()
        .ok()
        .and_then(|value| {
            value
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "OmniApp Project".to_owned());
    let name = requested_name.unwrap_or(&inferred_name);
    let quoted_name = serde_json::to_string(name)?;
    let config = format!(
        r##"version: 1
name: {quoted_name}
description: A local-first OmniApp project.
theme:
  accent: "#245c47"
  sidebar: "#17231e"
  background: "#f6f7f4"
navigation:
  - view: library
"##
    );
    fs::write(metadata.join("config.yml"), config)?;
    fs::write(metadata.join("models/book.yml"), DEFAULT_MODEL)?;
    fs::write(metadata.join("views/library.yml"), DEFAULT_VIEW)?;
    let site = metadata.join("sites/main");
    fs::create_dir_all(site.join("layouts"))?;
    fs::create_dir_all(site.join("includes"))?;
    fs::create_dir_all(site.join("pages"))?;
    fs::create_dir_all(site.join("assets"))?;
    fs::write(
        site.join("site.yml"),
        format!("version: 1\ntitle: {quoted_name}\n"),
    )?;
    fs::write(site.join("layouts/base.html"), DEFAULT_LAYOUT)?;
    fs::write(site.join("pages/index.html"), DEFAULT_INDEX_PAGE)?;
    fs::write(site.join("pages/book.html"), DEFAULT_RECORD_PAGE)?;
    fs::write(site.join("assets/main.css"), DEFAULT_SITE_CSS)?;
    fs::write(
        metadata.join("scripts/README.md"),
        "# Project scripts\n\nExecutable project automation will live here. Scripts should use the OmniApp API instead of editing record files directly.\n",
    )?;
    update_agents_file(path, name)?;
    update_gitignore(path)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "initialized": true,
                "name": name,
                "path": path,
            }))?
        );
    } else {
        println!("Initialized OmniApp project in {}", path.display());
        println!("Next: omniapp validate {}", path.display());
    }
    Ok(())
}

fn update_agents_file(root: &Path, project_name: &str) -> Result<()> {
    let path = root.join("AGENTS.md");
    let mut contents = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if contents.contains(AGENTS_SECTION_START) {
        return Ok(());
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    if !contents.is_empty() {
        contents.push('\n');
    }
    write!(
        contents,
        r"{AGENTS_SECTION_START}
# OmniApp project instructions

This directory is the **{project_name}** OmniApp project. It is agent-first: prefer the `omniapp` CLI for record operations so schema validation, relationships, indexing, and path moves remain consistent.

## Filesystem contract

- The filesystem is canonical. Git should track model definitions and project content.
- `.omniapp/config.yml` contains project configuration.
- `.omniapp/models/*.yml` defines record storage, fields, validation, and relationships.
- `.omniapp/views/*.yml` defines saved queries and presentation.
- `.omniapp/sites/<name>/` holds one public site each: layouts, includes, pages, and assets (minijinja templates).
- `.omniapp/scripts/` is reserved for project automation.
- `.omniapp/cache.sqlite3*` is generated and disposable; never edit or commit it.
- `_site/<name>/` is the built output per site; generated and disposable.
- Content outside `.omniapp/` is project data. Models determine whether each record is a directory or a single Markdown file.

## CLI workflow

Run commands from this project root:

```sh
omniapp validate
omniapp list <Model>
omniapp get <Model> <id-or-slug>
omniapp create <Model> --set field=value
omniapp update <Model> <id-or-slug> --set field=value
omniapp delete <Model> <id-or-slug>
omniapp query <view>
omniapp search <query>
omniapp relationships <Model> <id-or-slug>
omniapp outputs <Model> <id-or-slug>
omniapp serve
omniapp build
```

`omniapp serve` hosts every site on its own port (in name order from the base port) and the admin application on the next port. `omniapp build` renders each site to `_site/<name>/` for deployment; `--site <name>` builds one.

Add `--json` to any command when structured output is more useful. For larger writes, use `--input file.json` or pipe JSON to `--input -`.

Direct file edits are allowed and expected in a local-first project, especially for Markdown and large assets. After direct edits, run `omniapp validate`. Do not write to the SQLite cache or assume it is authoritative.
{AGENTS_SECTION_END}
"
    )?;
    fs::write(path, contents)?;
    Ok(())
}

fn update_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let mut contents = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let entries = [
        ".omniapp/cache.sqlite3",
        ".omniapp/cache.sqlite3-shm",
        ".omniapp/cache.sqlite3-wal",
        "_site/",
    ];
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    for entry in entries {
        if !contents.lines().any(|line| line.trim() == entry) {
            contents.push_str(entry);
            contents.push('\n');
        }
    }
    fs::write(path, contents)?;
    Ok(())
}

fn validate(path: &Path, full: bool, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let report = if full {
        workspace.validate_full()?
    } else {
        workspace.validate()?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for diagnostic in &report.diagnostics {
            println!(
                "{:?}: {}: {}",
                diagnostic.severity, diagnostic.location, diagnostic.message
            );
        }
        if report.is_valid() {
            println!(
                "Valid: {} model(s), {} view(s), {} record(s)",
                report.models, report.views, report.records
            );
        }
    }
    if !report.is_valid() {
        bail!(
            "validation failed with {} problem(s)",
            report.diagnostics.len()
        );
    }
    Ok(())
}

fn build(
    path: &Path,
    site: Option<String>,
    out: Option<PathBuf>,
    base_url: Option<String>,
    strict: bool,
    json: bool,
) -> Result<()> {
    if site.is_none() && (out.is_some() || base_url.is_some()) {
        bail!("--out and --base-url apply to a single site; pass --site <name>");
    }
    let workspace = Workspace::new(path);
    let reports = omniapp_site::build(
        &workspace,
        &omniapp_site::BuildOptions {
            site,
            out_dir: out,
            base_url,
            strict,
        },
    )?;
    let problems: usize = reports.iter().map(|(_, report)| report.errors.len()).sum();
    if json {
        let map = reports
            .iter()
            .map(|(name, report)| (name.clone(), report))
            .collect::<std::collections::BTreeMap<_, _>>();
        println!("{}", serde_json::to_string_pretty(&map)?);
    } else {
        for (name, report) in &reports {
            for problem in &report.errors {
                eprintln!("error: {name}: {}: {}", problem.location, problem.message);
            }
            if report.errors.is_empty() {
                println!(
                    "Built {name}: {} page(s) ({} from records), {} site asset(s), {} record asset(s) into {}",
                    report.pages + report.record_pages,
                    report.record_pages,
                    report.site_assets,
                    report.record_assets,
                    report.out_dir.display()
                );
            }
        }
    }
    if problems > 0 {
        bail!("site build failed with {problems} problem(s); existing output was left untouched");
    }
    Ok(())
}

async fn serve(path: PathBuf, port: u16, no_open: bool, no_admin: bool, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let synced = workspace.sync_cache()?;
    let report = workspace.validate_synced(&synced);
    if !report.is_valid() {
        for diagnostic in &report.diagnostics {
            eprintln!(
                "{:?}: {}: {}",
                diagnostic.severity, diagnostic.location, diagnostic.message
            );
        }
        bail!("project validation failed; fix the reported problems before serving");
    }
    let indexed = synced.records.len();
    let bound = omniapp_web::bind(workspace, port, !no_admin).await?;
    let sites = bound
        .site_addrs()
        .into_iter()
        .map(|(name, addr)| (name, format!("http://{addr}")))
        .collect::<Vec<_>>();
    let admin_url = bound.admin_addr().map(|addr| format!("http://{addr}"));
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "sites": sites.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
                "admin_url": admin_url,
                "indexed": indexed,
            }))?
        );
    } else {
        println!("OmniApp is running ({indexed} record(s) indexed)");
        let width = sites
            .iter()
            .map(|(name, _)| name.len())
            .chain(std::iter::once(5))
            .max()
            .unwrap_or(5);
        for (name, url) in &sites {
            println!("  {name:<width$}  {url}/");
        }
        if let Some(admin_url) = &admin_url {
            println!("  {:<width$}  {admin_url}/", "admin");
        }
    }
    let open_url = sites
        .first()
        .map(|(_, url)| url.clone())
        .or_else(|| admin_url.clone());
    if !no_open
        && let Some(open_url) = open_url
        && let Err(error) = open::that(&open_url)
    {
        eprintln!("Could not open the default browser: {error}");
    }
    bound.serve().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn initialized_project_is_valid_and_gitignore_is_preserved() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join(".gitignore"), "dist/\n").unwrap();
        initialize(directory.path(), Some("Test Library"), false).unwrap();

        let report = Workspace::new(directory.path()).validate().unwrap();
        assert!(report.is_valid());
        assert_eq!(report.models, 1);
        assert_eq!(report.views, 1);

        let site = directory.path().join(".omniapp/sites/main");
        assert!(site.join("site.yml").is_file());
        assert!(site.join("layouts/base.html").is_file());
        assert!(site.join("pages/index.html").is_file());
        assert!(site.join("pages/book.html").is_file());
        assert!(site.join("assets/main.css").is_file());

        let reports = omniapp_site::build(
            &Workspace::new(directory.path()),
            &omniapp_site::BuildOptions::default(),
        )
        .unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].1.errors.is_empty(), "{:?}", reports[0].1.errors);
        assert!(directory.path().join("_site/main/index.html").is_file());
        assert!(
            directory
                .path()
                .join("_site/main/assets/main.css")
                .is_file()
        );

        let ignore = fs::read_to_string(directory.path().join(".gitignore")).unwrap();
        assert!(ignore.starts_with("dist/\n"));
        assert!(ignore.contains(".omniapp/cache.sqlite3\n"));
        assert!(ignore.contains("_site/\n"));
        let agents = fs::read_to_string(directory.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("Test Library"));
        assert!(agents.contains("omniapp validate"));
    }

    #[test]
    fn initialization_refuses_to_overwrite_metadata() {
        let directory = tempdir().unwrap();
        initialize(directory.path(), None, false).unwrap();
        assert!(initialize(directory.path(), None, false).is_err());
    }

    #[test]
    fn initialization_preserves_existing_agent_instructions() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("AGENTS.md"), "# Existing rules\n").unwrap();
        initialize(directory.path(), Some("Test"), false).unwrap();
        let agents = fs::read_to_string(directory.path().join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("# Existing rules\n"));
        assert_eq!(agents.matches(AGENTS_SECTION_START).count(), 1);
    }
}
