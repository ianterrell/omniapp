# Architecture, implementation status, and roadmap

## Non-negotiable invariants

1. Project files are canonical. No application behavior may require the cache to recover a record.
2. `.omniapp/` describes the project; record and generated content live outside it.
3. All writes pass through the core service so path safety, validation, references, and indexes stay consistent.
4. Serialized definitions are versioned and reject unknown keys. Until the project format is declared stable, format changes may be intentionally breaking; after that declaration they require explicit compatibility or migration behavior.
5. The web server binds to loopback by default. A future remote mode needs an explicit trust and authentication design.

## Crate boundaries

| Crate | Owns | Must not own |
|---|---|---|
| `omniapp-schema` | Serializable project/model/view/site types and definition validation | Filesystem access beyond reading a definition |
| `omniapp-core` | Workspace discovery, record codecs, CRUD, revisions, watcher, reference/output resolution, queries, validation, caches | HTTP or terminal presentation |
| `omniapp-site` | Site config/page discovery, template environment, record context resolution, route table, static build pipeline | HTTP serving; record parsing or writing (delegates to core) |
| `omniapp-web` | Loopback HTTP API, guarded asset delivery, the admin application, live public-site serving | Direct project-file writes |
| `omniapp-cli` | Command parsing, initialization, build orchestration, process lifecycle | Record parsing or business invariants |

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
                         ^
                         |
              debounced filesystem watcher
```

Startup reconciles the cache with the filesystem instead of rebuilding it: one pruned directory walk discovers record locations for every model at once, stat-based fingerprints (`mtime_ns:size` per source file) decide which records are re-read, changed records are parsed in parallel, and everything else is served from the cache untouched. A digest of the model definitions is stored alongside the records; when definitions change, every record is re-read. The recursive filesystem watcher batches changes and refreshes only affected record locations. Web listings and saved-view queries execute against cached JSON through SQLite JSON functions; FTS queries use the incrementally maintained FTS5 table. Record detail, relationship, and asset lookups in the web server run against an in-memory `RecordsSnapshot` loaded from the cache and invalidated by the watcher (and immediately after API writes). CLI one-shot commands run the same fingerprint sync first, so they always observe canonical state — including direct file edits — while staying fast; `validate --full` bypasses the fingerprints, re-reads everything from disk, and rebuilds the cache. The cache remains derived and disposable: deleting `cache.sqlite3` costs one full scan, never data.

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
- Rebuildable SQLite record cache and FTS5 index with stat-fingerprint incremental sync.
- Debounced recursive filesystem watcher with per-record upsert/removal.
- SQLite-backed declarative filtering, ordering, null handling, and pagination.
- An embedding table whose contents are explicitly rebuildable.

`sqlite-vec` virtual tables and embedding providers remain pending because dimension/model lifecycle and extension packaging need a stable contract first.

### Phase 3: initial product surfaces — implemented

- `omniapp init`, `validate`, `serve`, `list`, `get`, `create`, `update`, `delete`, `query`, `search`, `relationships`, and `outputs`.
- Human-readable terminal output and stable JSON-shaped output for automation.
- Agent-first initialization that creates or safely extends a root `AGENTS.md`.
- Port probing from 7777 and automatic browser opening.
- Project/model/view/record/search/relationship/output HTTP endpoints plus guarded asset files.
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

### Phase 7: generated output resolution — implemented

- Named output templates resolve against validated record fields to safe project-relative paths.
- Resolution reports whether each destination currently exists as a file or directory.
- The Rust core, `omniapp outputs`, HTTP, and record editor share the same output result.
- OmniApp resolves destinations but does not claim generated artifacts as canonical record data.

### Phase 8: filesystem asset delivery — implemented

- Large assets are placed and versioned outside OmniApp using normal filesystem tools.
- Asset fields index only safe project-relative paths; SQLite never stores asset bytes.
- A guarded loopback route serves configured assets only, rejects `.omniapp` and symlink escapes, and supports HTTP byte ranges.
- The generated UI renders lazy image thumbnails, video/audio players, and links for other file types.

### Phase 9: the admin application — implemented

- Config-driven title and three-color theme; every derived shade is computed from the configured bases.
- Two-tier navigation: curated sidebar entries, optional tab groups, hidden-but-addressable views.
- Hash-routed record pages: a formatted read view with backreference tabs, full-page edit and create forms, deep links, and browser history. The modal editor is gone.
- Dedicated renderers for every view type: table, board, calendar, cards, timeline, tree, and form.
- A per-model display DSL: named blocks of layout nodes (grid/stack/card/section/field/resource/outputs) with responsive breakpoint maps and declarative field formats. The `detail` block lays out the record page (markdown rendered server-side by the site renderer); other named blocks render records as items in lists and `resource` collections. Client-side actions: copy-to-clipboard, navigate, create-with-prefill, and in-place checklist toggles.
- View-wide substring search executed in the SQLite cache alongside the view's filters.
- Served on its own port at `/`; each public site gets its own port.

### Phase 10: static site generation — implemented

- `.omniapp/sites/<name>/` holds one site each — minijinja layouts, includes, pages, and assets; a project can publish any number of sites from the same records. Page frontmatter is deliberately open (user space), unlike strict `.omniapp` definitions.
- Generator pages produce one URL per record from a model or saved view, with `{field}` permalinks validated like output templates.
- Templates see lazy record objects: reference fields resolve to records, backreferences via `inbound`, page URLs via `record.url`.
- `omniapp build` renders each site to a staging directory and only replaces `_site/<name>/` on success, collecting every page error; reserved prefixes (`assets/`, `files/`) and within-site URL collisions fail that site's build without stopping the others.
- `omniapp serve` renders every site live, one port per site plus one for the admin — the watcher invalidates the per-site model cache and the shared records snapshot on any change, so record, template, and config edits appear on the next request; template errors render a diagnostic page.
- Record assets are exposed as `/files/<project-relative>` in both modes; site assets as `/assets/...`.

## Remaining roadmap

### Production hardening

1. Add a crash-recoverable journal for writes that touch several files or move a directory and then rewrite its contents.
2. Add inter-process record locks so two CLI/server processes cannot pass revision checks and write concurrently.
3. Reconcile watcher state after event overflows, watcher errors, sleep/wake, and filesystems that need polling rather than native notifications.
4. Extend canonical-path and symlink containment checks from served assets to every configured record source and generated-output destination.
5. Add HTTP integration tests, watcher recovery tests, multi-process conflict tests, malformed-YAML preservation fixtures, and Windows/Linux CI.
6. Benchmark large workspaces and add optional expression indexes for fields frequently used by saved queries.

### Product and extension work

1. Add relationship traversal and backreference joins to declarative query filters, grouping, and ordering.
2. Add search result excerpts and highlighting (record pages already render Markdown via `format: markdown`).
3. Embed `sqlite-vec`; define an embedding-provider interface, dimension changes, background indexing, and fully rebuildable semantic search.
4. Add a sandboxed script host and event hooks (giving the reserved `action_group` display node its meaning). Scripts must call application services rather than edit canonical files directly.
5. Add listing-page pagination to the site generator (`views.*` and full record lists cover current needs).
6. Add optional derived thumbnails/posters for very large media. Current previews stream original files and size them in the browser.
7. Declare a stable project format when appropriate, then add migrations and a compatibility corpus.
8. Add packaging, installers, shell completions, CI, and signed release automation.

Remote serving remains out of scope. If introduced, it requires authentication, authorization, CSRF protection, and a different asset trust model; loopback-only assumptions must not silently carry over.

## Scripting boundary

`.omniapp/scripts/` is created today but execution is intentionally deferred. The script host should receive a capability-scoped client exposing `query`, `get`, `create`, `update`, `delete`, and generated-output path resolution. It should use the same service calls as HTTP handlers. The host needs defined limits for runtime, memory, environment variables, network access, and subprocesses; picking a language runtime before defining those security semantics would make the project format less stable.

## Generated outputs

Models name output path templates under `outputs`. For example, `publication: build/{slug}` establishes a discoverable destination without putting executable behavior into a model. `omniapp outputs`, the core service, and HTTP resolve templates for a record and inspect the current filesystem destination. Output paths are project-relative, cannot enter `.omniapp`, and cannot traverse above the project root.

Outputs and site permalinks are deliberately independent. `outputs` declares where external pipelines (scripts, encoders) put artifacts a record expects — an episode's `mp3`, a transcript. Site permalinks live in `.omniapp/site/pages/` because a record's public URL is presentation, not model data; models never reference the site.

## Concurrency and failures

- A record file is written to a sibling temporary file and renamed into place.
- Updates and deletes compare the caller's record revision before touching the filesystem.
- A path-field change renames the record file or directory before rewriting configured fields.
- Unknown keys and untouched formatting/comments in shared YAML and frontmatter are preserved.
- Record creation/update is validated against the complete relationship graph before files change.
- Deletion is rejected when another record references the target or a nested model record would be removed.

Multi-file updates are not yet a single crash-safe transaction. The longer-term design is a small filesystem journal under `.omniapp/transactions/` containing intended renames and hashes, recoverable on workspace load.
