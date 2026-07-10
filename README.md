# OmniApp

OmniApp is a local-first platform for arbitrary structured data. A project is a normal directory: human-readable YAML, Markdown, and asset files are the source of truth, while `.omniapp/` contains declarative definitions and a disposable SQLite cache.

This repository contains the first working vertical slice of the architecture:

- declarative models with path-templated, YAML, Markdown, and asset fields;
- nested storage such as `books/{book}/scenes/{slug}`;
- declarative views with filters, ordering, and pagination;
- record/schema/reference validation;
- atomic filesystem CRUD and safe relationship-aware deletion;
- rebuildable SQLite FTS5 search cache and an embedding-ready cache table;
- a local-only HTTP API and schema-generated table/form interface;
- `init`, `validate`, and `serve` CLI commands.

## Quick start

```sh
cargo run -p omniapp-cli -- init ./my-project --name "My Project"
cargo run -p omniapp-cli -- validate ./my-project
cargo run -p omniapp-cli -- serve ./my-project
```

`serve` starts at `127.0.0.1:7777`, increments until it finds a free port, rebuilds the cache, and opens the default browser. Pass `--no-open` for headless use.

## Repository layout

```text
crates/
  omniapp-schema/  Stable serialized definitions and schema validation
  omniapp-core/    Workspace, record I/O, validation, query, and cache services
  omniapp-web/     Local HTTP API and generated browser interface
  omniapp-cli/     The omniapp executable
docs/
  architecture.md  Boundaries, invariants, and delivery roadmap
  project-format.md
```

The current implementation deliberately does not execute project scripts yet, load `sqlite-vec`, or render specialized board/calendar/tree layouts. Their boundaries and sequencing are documented in [the architecture plan](docs/architecture.md).

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The cache and its WAL sidecars belong in `.gitignore`. Delete `.omniapp/cache.sqlite3*` at any time; `omniapp serve` recreates it from project files.

