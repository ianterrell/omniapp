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

The server rereads records for requests. Search rebuilds before querying so edits made in an external editor are visible even before a filesystem watcher is introduced. This favors correctness in the initial release; watcher-driven incremental indexing is the planned optimization.

## Delivered phases

### Phase 1: stable project foundation — implemented

- Cargo workspace and reusable crate separation.
- Versioned, strict YAML project format.
- Configurable path templates with nested record support.
- Multiple YAML and Markdown files per record, plus fixed-name asset fields.
- Type, required, range, length, regex, enum, duplicate-key, and relationship validation.
- Atomic writes, path-field directory moves, and guarded deletes.

### Phase 2: query and cache — implemented foundation

- Declarative `eq`, `not_eq`, comparison, containment, membership, and null filters.
- Multi-field ascending/descending ordering and pagination.
- Rebuildable SQLite record cache and FTS5 index.
- An embedding table whose contents are explicitly rebuildable.

`sqlite-vec` virtual tables and embedding providers remain pending because dimension/model lifecycle and extension packaging need a stable contract first.

### Phase 3: initial product surfaces — implemented

- `omniapp init`, `validate`, and `serve`.
- Port probing from 7777 and automatic browser opening.
- Project/model/view/record/search HTTP endpoints.
- Schema-driven table display and generated create/edit/delete forms.

### Phase 4: next implementation

1. Add a debounced filesystem watcher and incremental cache transactions.
2. Add optimistic concurrency using file fingerprints so external edits cannot be silently overwritten by a stale form.
3. Add asset upload/rename APIs and codecs for richer media metadata.
4. Implement specialized tree, board, calendar, gallery, and timeline renderers against the existing view/query contract.
5. Embed `sqlite-vec`; define an embedding-provider interface, dimension migration, and background job state.
6. Add a sandboxed script host with capability grants. Scripts will call application services, never raw filesystem primitives.
7. Add format migrations and a compatibility test corpus before releasing format version 2.

## Scripting boundary

`.omniapp/scripts/` is created today but execution is intentionally deferred. The script host should receive a capability-scoped client exposing `query`, `get`, `create`, `update`, `delete`, and generated-output path resolution. It should use the same service calls as HTTP handlers. The host needs defined limits for runtime, memory, environment variables, network access, and subprocesses; picking a language runtime before defining those security semantics would make the project format less stable.

## Generated outputs

Models can name output path templates under `outputs`. For example, `publication: build/{slug}` establishes a discoverable destination without putting executable behavior into a model. A script or future CLI command resolves the template for a record and writes the artifact there. Output paths are project-relative and cannot traverse above the project root.

## Concurrency and failures

- A record file is written to a sibling temporary file and renamed into place.
- A path-field change renames the whole record directory before rewriting configured fields.
- Unknown keys in shared YAML documents are preserved.
- Record creation/update is validated against the complete relationship graph before files change.
- Deletion is rejected when another record references the target or a nested model record would be removed.

Multi-file updates are not yet a single crash-safe transaction. The longer-term design is a small filesystem journal under `.omniapp/transactions/` containing intended renames and hashes, recoverable on workspace load.

