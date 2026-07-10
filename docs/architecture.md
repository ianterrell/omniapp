# Architecture and implementation plan

## Non-negotiable invariants

1. Project files are canonical. No application behavior may require the cache to recover a record.
2. `.omniapp/` describes the project; record and generated content live outside it.
3. All writes pass through the core service so path safety, validation, references, and indexes stay consistent.
4. Serialized definitions are versioned and reject unknown keys. Format changes require explicit compatibility or migration behavior.
5. The web server binds to loopback by default. A future remote mode needs an explicit trust and authentication design.

## Crate boundaries

| Crate | Owns | Must not own |
|---|---|---|
| `omniapp-schema` | Serializable project/model/view types and definition validation | Filesystem access beyond reading a definition |
| `omniapp-core` | Workspace discovery, record codecs, CRUD, reference resolution, queries, validation, caches | HTTP or terminal presentation |
| `omniapp-web` | Loopback HTTP API and generated UI | Direct project-file writes |
| `omniapp-cli` | Command parsing, initialization, process lifecycle | Record parsing or business invariants |

This direction lets future desktop shells, language bindings, and background services reuse the same core API.

## Data flow

```text
models + project files
          |
          v
  schema and codecs ---> validation/reference resolution
          |                         |
          v                         v
    application services <--- CLI / local HTTP API
          |
          +------> atomic project-file writes
          |
          `------> disposable SQLite (records, FTS5, embeddings)
```

The server performs one complete cache build at startup, then a recursive filesystem watcher batches changes and refreshes only affected record locations. Model, view, or project configuration changes intentionally trigger a complete rebuild. Web listings and saved-view queries execute against cached JSON through SQLite JSON functions; FTS queries use the incrementally maintained FTS5 table. CLI reads remain filesystem-direct so one-shot commands always observe canonical state.

## Delivered phases

### Phase 1: stable project foundation — implemented

- Cargo workspace and reusable crate separation.
- Versioned, strict YAML project format.
- Configurable path templates with nested record support.
- Directory records with multiple YAML/Markdown files and arbitrary configured filenames.
- Single-file Markdown records with structured YAML frontmatter.
- Type, required, range, length, regex, enum, duplicate-key, and relationship validation.
- Atomic writes, path-field file/directory moves, and guarded deletes.

### Phase 2: query, watcher, and cache — implemented

- Declarative `eq`, `not_eq`, comparison, containment, membership, and null filters.
- Multi-field ascending/descending ordering and pagination.
- Rebuildable SQLite record cache and FTS5 index.
- Debounced recursive filesystem watcher with per-record upsert/removal.
- SQLite-backed declarative filtering, ordering, null handling, and pagination.
- An embedding table whose contents are explicitly rebuildable.

`sqlite-vec` virtual tables and embedding providers remain pending because dimension/model lifecycle and extension packaging need a stable contract first.

### Phase 3: initial product surfaces — implemented

- `omniapp init`, `validate`, `serve`, `list`, `get`, `create`, `update`, `delete`, `query`, and `search`.
- Human-readable terminal output and stable JSON-shaped output for automation.
- Port probing from 7777 and automatic browser opening.
- Project/model/view/record/search HTTP endpoints.
- Schema-driven table display and generated create/edit/delete forms.

### Phase 4: optimistic concurrency — implemented

- Every record read includes a SHA-256 revision of files OmniApp can rewrite.
- Updates and deletes require the revision observed by the caller.
- Stale HTTP mutations return `409 Conflict`; the CLI submits a freshly read revision automatically.
- Configured large asset files are excluded from revision hashing because OmniApp does not rewrite their bytes.

### Phase 5: format-preserving YAML writes — implemented

- Standalone YAML and Markdown frontmatter use the same targeted top-level mapping editor.
- Untouched keys, key ordering, blank lines, comments, quoting, and nested blocks remain byte-for-byte unchanged.
- Updated keys retain inline comments while their value representation is regenerated.
- Unknown fields continue to round-trip without becoming part of the model.

### Phase 6: relationship services — implemented

- Reference fields resolve to complete outbound record links.
- Inbound backreferences are derived across every model without duplicating them in project files.
- Referenced target fields must be unique, preventing ambiguous graph edges.
- Traversal is available through the Rust core, `omniapp relationships`, HTTP, and the record editor.

### Phase 7: next implementation

1. Add generated-output resolution.
2. Serve configured filesystem assets with schema-driven media previews.
3. Implement relationship joins in declarative filters and specialized tree, board, calendar, gallery, and timeline renderers.
4. Embed `sqlite-vec`; define an embedding-provider interface, dimension migration, and background job state.
5. Add a sandboxed script host with capability grants. Scripts will call application services, never raw filesystem primitives.

## Scripting boundary

`.omniapp/scripts/` is created today but execution is intentionally deferred. The script host should receive a capability-scoped client exposing `query`, `get`, `create`, `update`, `delete`, and generated-output path resolution. It should use the same service calls as HTTP handlers. The host needs defined limits for runtime, memory, environment variables, network access, and subprocesses; picking a language runtime before defining those security semantics would make the project format less stable.

## Generated outputs

Models can name output path templates under `outputs`. For example, `publication: build/{slug}` establishes a discoverable destination without putting executable behavior into a model. A script or future CLI command resolves the template for a record and writes the artifact there. Output paths are project-relative and cannot traverse above the project root.

## Concurrency and failures

- A record file is written to a sibling temporary file and renamed into place.
- Updates and deletes compare the caller's record revision before touching the filesystem.
- A path-field change renames the record file or directory before rewriting configured fields.
- Unknown keys and untouched formatting/comments in shared YAML and frontmatter are preserved.
- Record creation/update is validated against the complete relationship graph before files change.
- Deletion is rejected when another record references the target or a nested model record would be removed.

Multi-file updates are not yet a single crash-safe transaction. The longer-term design is a small filesystem journal under `.omniapp/transactions/` containing intended renames and hashes, recoverable on workspace load.
