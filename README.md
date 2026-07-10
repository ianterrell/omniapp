# OmniApp

OmniApp is a local-first platform for arbitrary structured data. A project is a normal directory: human-readable YAML, Markdown, and asset files are the source of truth, while `.omniapp/` contains declarative definitions and a disposable SQLite cache.

This repository contains the first working vertical slice of the architecture:

- directory records with arbitrary configured YAML, Markdown, and asset filenames;
- single-file Markdown records such as `posts/{slug}.md` with structured YAML frontmatter;
- nested storage such as `books/{book}/scenes/{slug}`;
- declarative views with filters, ordering, and pagination;
- debounced filesystem watching with incremental cache and FTS updates;
- cache-backed declarative queries for the local web application;
- record/schema/reference validation;
- atomic filesystem CRUD and safe relationship-aware deletion;
- rebuildable SQLite FTS5 search cache and an embedding-ready cache table;
- a local-only HTTP API and schema-generated table/form interface;
- complete CLI record access with human-readable and JSON output.

## Quick start

```sh
cargo run -p omniapp-cli -- init ./my-project --name "My Project"
cargo run -p omniapp-cli -- validate ./my-project
cargo run -p omniapp-cli -- serve ./my-project
```

Initialization also creates a root `AGENTS.md` with the project structure, filesystem rules, and CLI workflow so coding agents can work safely without prior OmniApp context. Existing agent instructions are preserved and the OmniApp section is appended once.

`serve` starts at `127.0.0.1:7777`, increments until it finds a free port, rebuilds the cache, and opens the default browser. Pass `--no-open` for headless use.

## CLI record access

All record commands operate directly on project files and do not require `omniapp serve`:

```sh
cd my-project

omniapp list Book
omniapp get Book dune
omniapp create Book \
  --set slug=dune \
  --set title=Dune \
  --set 'author=Frank Herbert'
omniapp update Book dune --set status=complete
omniapp query library --page 1
omniapp search 'desert OR ecology'
omniapp delete Book dune
```

Selectors accept the canonical record key/path or a unique `id` or `slug`. `--set` decodes valid JSON values, so booleans, numbers, arrays, objects, and `null` retain their types; other input is treated as a string. Quote a numeric-looking string as JSON, for example `--set 'title="1984"'`. On update, `null` removes an optional field.

Every command accepts `--json` before or after the subcommand. List and query output includes pagination metadata:

```sh
omniapp list Book --json | jq '.records[].values.title'
omniapp get Book dune --json
omniapp search ecology --json
```

Create and update also accept a plain JSON object from a file or stdin. Repeatable `--set` values override fields supplied by `--input`:

```sh
omniapp create Book --input book.json --json

printf '%s' '{"slug":"dune","title":"Dune"}' \
  | omniapp create Book --input - --json
```

When operating outside the project directory, append its path after the command arguments, such as `omniapp list Book /path/to/project`.

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

[`examples/markdown-records`](examples/markdown-records) is a working project showing both single-file Markdown records and directory records whose body/frontmatter document is named `content.md`.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The cache and its WAL sidecars belong in `.gitignore`. Delete `.omniapp/cache.sqlite3*` at any time; `omniapp serve` recreates it from project files.
