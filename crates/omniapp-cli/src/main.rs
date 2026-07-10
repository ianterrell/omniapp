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
    },
    /// Start the local web application.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// First port to try. OmniApp increments until one is available.
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Do not open the default browser.
        #[arg(long)]
        no_open: bool,
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
        Command::Validate { path } => validate(&path, cli.json),
        Command::Serve {
            path,
            port,
            no_open,
        } => serve(path, port, no_open, cli.json).await,
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
    fs::write(
        metadata.join("config.yml"),
        format!("version: 1\nname: {quoted_name}\ndescription: A local-first OmniApp project.\n"),
    )?;
    fs::write(metadata.join("models/book.yml"), DEFAULT_MODEL)?;
    fs::write(metadata.join("views/library.yml"), DEFAULT_VIEW)?;
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
- `.omniapp/scripts/` is reserved for project automation.
- `.omniapp/cache.sqlite3*` is generated and disposable; never edit or commit it.
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
omniapp serve
```

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

fn validate(path: &Path, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let report = workspace.validate()?;
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

async fn serve(path: PathBuf, port: u16, no_open: bool, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let report = workspace.validate()?;
    if !report.is_valid() {
        for diagnostic in &report.diagnostics {
            eprintln!(
                "{:?}: {}: {}",
                diagnostic.severity, diagnostic.location, diagnostic.message
            );
        }
        bail!("project validation failed; fix the reported problems before serving");
    }
    let indexed = workspace.rebuild_cache()?;
    let listener = omniapp_web::bind_available(port).await?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}");
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "url": url,
                "indexed": indexed,
            }))?
        );
    } else {
        println!("OmniApp is running at {url} ({indexed} record(s) indexed)");
    }
    if !no_open && let Err(error) = open::that(&url) {
        eprintln!("Could not open the default browser: {error}");
    }
    omniapp_web::serve(workspace, listener).await?;
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

        let ignore = fs::read_to_string(directory.path().join(".gitignore")).unwrap();
        assert!(ignore.starts_with("dist/\n"));
        assert!(ignore.contains(".omniapp/cache.sqlite3\n"));
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
