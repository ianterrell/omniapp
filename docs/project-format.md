# Project format version 1

All paths below are relative to the project root.

## Configuration

`.omniapp/config.yml`:

```yaml
version: 1
name: Editorial calendar
description: Local publishing workflow
theme:
  accent: "#245c47"
  sidebar: "#17231e"
  background: "#f6f7f4"
navigation:
  - view: dashboard
  - label: Posts
    views: [posts, published, pipeline]
```

`name` is the application title shown in the admin sidebar; `description` appears beneath it.

`theme` is optional. Each color is a 6-digit hex value; unset colors fall back to defaults. All other shades — hover states, muted sidebar text, the soft accent tint — are derived from these three bases, with a luminance guard so light sidebar colors keep readable text.

`navigation` is optional. When absent, every view appears in the sidebar. When present it is authoritative: each entry is either a single view (`view: name`, labeled by the view unless `label` overrides it) or a labeled group (`label` + `views`) rendered as one sidebar item with horizontal tabs for its views. A view may appear at most once across all entries. Views left out of `navigation` are hidden from the sidebar but remain addressable by URL.

Model and view files are loaded from `.omniapp/models/*.{yml,yaml}` and `.omniapp/views/*.{yml,yaml}`. Names must be unique within each definition type.

## Models

Every model chooses either directory or single-file storage. Storage paths may contain one `{field}` placeholder per path segment. Placeholders may be embedded in a filename, such as `{slug}.md`, and each requires a matching `path` source.

### Directory records

A directory record can use any filenames declared by the model. There is no `index.md` or other naming convention:

```yaml
version: 1
name: Scene
storage:
  kind: directory
  path: books/{book}/scenes/{slug}
fields:
  book:
    type: reference
    required: true
    source: { kind: path, variable: book }
    reference: { model: Book, field: slug }
  slug:
    type: string
    required: true
    source: { kind: path, variable: slug }
  title:
    type: string
    required: true
    source: { kind: yaml, file: scene.yml, key: title }
  position:
    type: integer
    source: { kind: yaml, file: scene.yml, key: position }
    validation: { min: 1 }
  draft:
    type: text
    source: { kind: markdown, file: draft.md }
outputs:
  rendered: build/{book}/scenes/{slug}.html
  build: { path: "build/{book}", kind: directory }
```

Markdown bodies and YAML frontmatter can share any configured document:

```yaml
version: 1
name: Article
storage:
  kind: directory
  path: articles/{slug}
fields:
  slug:
    type: string
    required: true
    source: { kind: path, variable: slug }
  title:
    type: string
    required: true
    source: { kind: frontmatter, file: manuscript.markdown, key: title }
  tags:
    type: json
    source: { kind: frontmatter, file: manuscript.markdown, key: tags }
  body:
    type: text
    source: { kind: markdown, file: manuscript.markdown }
```

This produces `articles/<slug>/manuscript.markdown`. Frontmatter fields and the body are parsed and written together atomically. Unknown frontmatter keys are retained.

OmniApp patches configured top-level YAML/frontmatter keys instead of serializing the complete mapping. Comments, ordering, blank lines, quoting, and nested formatting outside a changed key remain untouched. A changed key keeps its inline comment, but its value is rendered in OmniApp's standard YAML style.

### Single-file records

For a single-file record, the storage path is the Markdown document itself. Markdown and frontmatter sources omit `file` because they implicitly use that document:

```yaml
version: 1
name: Post
storage:
  kind: file
  path: posts/{slug}.md
fields:
  slug:
    type: string
    required: true
    source: { kind: path, variable: slug }
  title:
    type: string
    required: true
    source: { kind: frontmatter, key: title }
  published_on:
    type: date
    source: { kind: frontmatter, key: published_on }
  body:
    type: text
    source: { kind: markdown }
```

The resulting file is ordinary Markdown:

```markdown
---
title: A local-first post
published_on: 2026-07-10
---
# The body
```

Single-file models intentionally support only `path`, `frontmatter`, and `markdown` sources. Data that needs several YAML, Markdown, or asset files should use directory storage.

Supported field types are `string`, `text`, `integer`, `number`, `boolean`, `date`, `date_time`, `enum`, `reference`, `asset`, and `json`.

Sources:

- `path`: a value captured from a placeholder in `storage.path`;
- `yaml`: a named key in a mapping; fields may share or use different YAML files;
- `frontmatter`: a named key in a Markdown document's YAML frontmatter;
- `markdown`: the UTF-8 Markdown body after any frontmatter;
- `asset`: the project-relative path to a fixed-name file when that file exists.

Assets remain normal project files and are never copied into SQLite. Directory records can use an `asset` source with a configured filename; frontmatter can also hold a project-relative path in a field whose type is `asset`. Paths must stay inside the project and outside `.omniapp`. The local web application serves only paths currently referenced by asset fields, with byte-range support for large video and audio files.

Validation supports `min`, `max`, `min_length`, `max_length`, `pattern`, and `choices`. Dates use `YYYY-MM-DD`; date-times use RFC 3339.

Reference fields resolve in both directions. For example, a `Scene.book` reference to `Book.slug` appears as an outbound relationship on the scene and an inbound backreference on the book. Values used as relationship targets must be unique within their model. Use `omniapp relationships Scene opening --json` or the record relationships HTTP endpoint to traverse the graph.

Record identity is the string or integer `id` field when present, otherwise the project-relative record file or directory. An explicit stable `id` is recommended when path fields are frequently renamed or other systems retain record URLs.

Every record returned by the core, CLI JSON output, or HTTP API also has a `revision`. Update bodies include that revision, and delete requests pass it as a query parameter. A revision mismatch means project files changed after the caller read them; OmniApp rejects the mutation instead of overwriting those changes.

A model may declare `parent: <field>`, naming one of its reference fields, to mark it as nested: its records belong to that parent record. The admin renders a breadcrumb trail through the parent chain on record pages (walking `parent` fields upward) ending in the record's own title, and nested models are conventionally reached through their parent's display panels rather than given top-level navigation entries.

A model may declare `title: <field>`, naming the field whose value titles a record wherever one is shown — breadcrumbs, page headers, cards, checklist rows. Without it, titles fall back to a heuristic: a `title` field, then `name`, then the first required string field with a value, then the record key.

Rails-style nested admin routes come from `route: <segment>` plus `identity: <field>`. A root model renders at `#/<route>/<identity value>` (`#/books/dune`); a nested model chains under its parent (`#/books/dune/docs/outline`), to any depth. Validation requires root routes to be unique, sibling routes to be distinct, and — so URL segments can resolve records — each routed child's back-reference to target its parent's declared `identity`. Records without a routed chain keep `#/records/<Model>/<key>` URLs, and legacy URLs rewrite themselves to the canonical form.

A model may declare `tabs:` — navigation underneath each record. Every tab is `{ label, block, route? }`: `block` names one of the model's display blocks (rendered in the record's context; `detail` may fall back to the built-in default page), and `route` (default: the block name) is the URL segment (`#/books/dune/changes`). The first tab lives at the record's bare URL. A tab route may deliberately match a nested child model's route so the collection tab and its member pages share a path (`…/changes` lists, `…/changes/10` shows one).

Named `outputs` are safe project-relative templates. Placeholders must name model fields and may appear in filenames. An output is a bare template (a single generated file, the common case) or the detailed form `{ path: "build/{slug}", kind: directory }`; a `directory` output additionally enumerates every file actually inside it (recursive, hidden entries skipped, sorted) at resolve time, so the record page reflects whatever the build actually produced. `omniapp outputs Book dune --json` returns every resolved path plus whether it currently exists as a file or directory, and the enumerated `files` for directory outputs; the generated bytes remain ordinary filesystem artifacts rather than record data.

## Views and queries

This view answers “all social posts scheduled but not posted, ordered by date, paginated”:

```yaml
version: 1
name: publishing-queue
label: Publishing queue
model: SocialPost
type: table
fields: [channel, copy, scheduled_for]
query:
  filters:
    - { field: status, op: eq, value: scheduled }
    - { field: posted_at, op: is_null }
  order:
    - { field: scheduled_for, direction: asc }
  page_size: 25
```

Filter operators are `eq`, `not_eq`, `lt`, `lte`, `gt`, `gte`, `contains`, `in`, `is_null`, and `is_not_null`. Null operators omit `value`; all others require it. Page sizes must be between 1 and 1000.

Each view type has a dedicated renderer in the admin application:

- `table`: sortable columns from `fields`; reference cells link to the referenced record's page.
- `board`: kanban columns from the `group_by` field's enum choices (plus a trailing column for records without a value, and appended columns for unexpected values).
- `cards`: a card grid; each record renders through its display block (see Display below), falling back to a built-in card whose image is the first `asset` field.
- `calendar`: a month grid keyed on the first date-typed `query.order` field (else the first date-typed view field); undated records collect in a strip below.
- `timeline`: a chronological list with date markers, in query order.
- `tree`: threaded indentation using `group_by` naming a self-reference field (for example a comment's `parent`).
- `form`: a full-page create form for the model.

Rows and cards navigate to a full record page — a formatted read view laid out by the model's `detail` display block (see Display below), or by a default stacked field grid with the record's relationships as tabs and an edit button.

Record list endpoints (`/api/views/{view}/records` and `/api/models/{model}/records`) accept `page`, `page_size`, and `q`. The `q` parameter performs a case-insensitive substring match over every string value and the record key, across the entire result set — combined with the view's filters, so totals and pagination reflect the match. (Non-ASCII case folding is not applied.) This is distinct from `/api/search`, which uses FTS5 syntax across all models. `GET /api/models/{model}/record?key=` accepts a canonical key, a storage path, or a unique `id`/`slug` value, mirroring CLI selectors.

When serving, the admin application runs on its own port at `/`; each public site (see below) runs on its own port.

## Display

Models control how the admin presents their records through named **display blocks** under `display:`. A block is a tree of layout nodes (or a list of nodes, rendered as an implicit vertical stack). Two names are special:

- `detail` lays out the record page.
- `card` is the default block used wherever records render as items — `cards` views, board columns, related-record tabs, and `resource` nodes on other models' pages. A view can pick a different block with `display: { item: <name> }`; a `resource` node with `item: <name>`.

```yaml
# .omniapp/models/book.yml
display:
  detail:
    - type: grid
      columns: { default: 1, lg: 12 }
      gap: lg
      children:
        - type: stack
          span: { lg: 8 }
          gap: lg
          children:
            - type: card
              children:
                - type: grid
                  columns: { default: 1, md: 2 }
                  gap: { column: xl, row: lg }
                  children:
                    - { type: field, name: author, empty: No author specified }
                    - { type: field, label: State, format: badges,
                        fields: [writing_state, publication_state] }
                    - { type: field, name: links, label: Store links, format: links }
                    - { type: field, name: meta.path, label: Storage path, format: code }
                - type: divider
                - type: field
                  name: description
                  format: markdown
                  actions: [{ type: copy, label: Copy HTML, value: html }]
        - type: stack
          span: { lg: 4 }
          gap: lg
          children:
            - type: resource
              model: Todo
              title: Todos
              display: checklist
              check: done
              order: [{ field: position }]
              limit: 5
              actions: [{ type: create, label: Add a todo }]
            - { type: outputs, title: Build artifacts, display: table }
  card:
    type: card
    children:
      - { type: field, name: title, format: title }
      - { type: field, name: publication_state, format: badge }
```

Node types:

- `grid`: a responsive column grid. `columns` is 1–12, either fixed (`columns: 2`) or a breakpoint map (`columns: { default: 1, md: 2, xl: 3 }`; unset breakpoints inherit the nearest smaller one — breakpoints are `sm` 640px, `md` 768px, `lg` 1024px, `xl` 1280px). Direct children may set `span` (same forms) to occupy several columns. `gap` is a spacing token `sm|md|lg|xl`, or `{ column, row }`.
- `stack`: children in a vertical flow, with `gap`.
- `card`: a boxed panel around its children, with optional `title` and `padding` (token).
- `section`: an unboxed group under a required `title` heading.
- `divider`: a horizontal rule.
- `field`: one record value with a label above it. `name` is a model field or `meta.key`/`meta.path`/`meta.model`; `label` overrides the field's label (`label: ""` hides the label row); `empty` replaces the em-dash placeholder. `format` overrides the type-derived rendering: `text`, `title` (heading, no label), `markdown` (rendered to HTML exactly as the site generator would), `code`, `date`, `relative_time`, `badge`/`badges` (with `fields:` listing several values as one badge row), `chips`, `list`, `links` (a JSON label→URL map), or `template` (with `template: "v{{ value }} pending"`). `actions` may include `{ type: copy, label, value: text|html|lines|json }` for copy-to-clipboard buttons (`html` renders markdown first, `lines` joins arrays with newlines).
- `resource`: records of another model that reference this record. `model` names the related model; `via` names its reference field when it has several pointing here. `display` is `item` (each record through a block: `item:` names one, else that model's `card` block, else the built-in card; `columns` arranges them), `table` (`fields:` lists columns, each a field name or `{ field, label, format }`), `checklist` (`check:` names a boolean field on the related model, toggled in place through the update API), or `summary` (a count). For `table`, `expand:` takes display nodes rendered in the related record's own context inside a per-row expand/contract disclosure — e.g. `expand: [{ type: field, name: body, label: "", format: markdown }]` to read documents in place. `order` and `limit` shape the list; `empty` replaces the empty-state text. `actions` may include `{ type: create, label }` (opens the related model's create form with the back-reference prefilled) and `{ type: navigate, label, view }`; adding `filtered: true` to a navigate pins the target view (which must show the related model) to this record via an equality filter on the back-reference (`#/views/todos?f.book=small`; the records API accepts the same as `filter=` JSON on top of the view's query).
- `outputs`: the record's generated artifacts, as `table` or `list`.
- `action_group` is reserved for a future pass and is rejected by validation for now.

Validation checks the whole tree: field names must exist, spans must fit their grid's columns at every breakpoint, `resource` targets must reference the model, checklist fields must be booleans, and named blocks must exist where referenced. Diagnostics carry the node's tree path (`model Book.display.detail.children[1]…`).

Records without a `detail` block get a default page: every field as a label-over-value cell in a responsive two-column grid (long-form values span the full width), followed by relationship tabs and generated outputs.

## Sites

A project can publish any number of fully user-styled public sites from the same records. Each site lives in `.omniapp/sites/<name>/` (names use lowercase letters, digits, and hyphens; the directory listing is the registry — there is no site list in config.yml). Sites are rendered with [minijinja](https://docs.rs/minijinja) (Jinja2 syntax):

```text
.omniapp/sites/<name>/
  site.yml          # optional site configuration
  layouts/*.html    # base templates ({% extends "layouts/base.html" %})
  includes/*.html   # partials ({% include "includes/post-card.html" %})
  pages/**          # URL-mapped pages (.html and .md)
  assets/**         # static files, served and copied verbatim under /assets/
```

`omniapp serve` renders every site live, one port per site in name order starting at the base port, with the admin application on the next port (edits to records, templates, or configuration appear on the next request). `omniapp build` writes one deployable static tree per site to `_site/<name>/` — without the admin application — and a site whose pages error keeps its previous output untouched; `omniapp build --site <name>` builds one site (and unlocks `--out`/`--base-url`). Two sites may publish the same model at different permalinks; `record.url` inside templates always reflects the site being rendered.

`site.yml` accepts `version`, `title` (defaults to the project `name`), `description`, `url` (absolute base URL for feeds/canonical links), and `params`, a free-form mapping exposed to templates as `site.params`.

### Pages

`pages/index.html` maps to `/`; every other page gets a pretty URL: `pages/about.md` becomes `/about/`, `pages/docs/setup.md` becomes `/docs/setup/`. `pages/404.html` is rendered as the not-found page. Pages may begin with a `---` YAML frontmatter block. Unlike `.omniapp` definitions, page frontmatter allows unknown keys — they are user space, exposed to the template as `page.<key>`. Recognized keys: `title`, `permalink` (overrides the derived URL), `layout` (wraps the rendered page in `layouts/<name>.html`, which receives it as `content`), and the generator keys below. `.md` pages are Jinja-rendered, converted from Markdown, then wrapped by their layout; `.html` pages normally use `{% extends %}` instead of `layout`.

A page that declares a record source becomes a generator, producing one page per record:

```html
---
view: published            # a saved view: its model, filters, and order
permalink: posts/{slug}/   # required; {field} placeholders, same rules as outputs
---
{% extends "layouts/post.html" %}
{% block main %}<h1>{{ record.title }}</h1>{{ record.body | markdown }}{% endblock %}
```

Alternatively declare `model: Post` with optional `filters:`/`order:` (the view query schema). `view` and `model` are mutually exclusive. A trailing `/` in the permalink produces `<path>/index.html`.

### Template context

Every render sees `site` (configuration plus `time`), `records.<Model>` (all records of a model), `views.<name>` (a saved view's records, unpaginated), and `page`; generator pages add `record`, and layout wraps add `content`. Record values resolve lazily: reference fields yield the referenced record objects (`post.author.name`, `post.tags`), `record.url` yields the record's generated page URL, `record.inbound.<Model>` yields backreferences (an author's posts as `author.inbound.Post`), and `record.meta.key/path/model` expose identity.

Filters: `markdown` (CommonMark plus tables, strikethrough, and footnotes), `asset_url` (turns an asset field value into its `/files/...` URL, identical in live serving and builds), `date(format)` (strftime over dates and RFC 3339 date-times), and `where`/`where_not` (filter record lists by field equality); minijinja built-ins such as `sort`, `selectattr`, and `groupby` also apply.

URL paths under `assets/` and `files/` are reserved; a page or permalink that resolves into them is a build error, as is any URL claimed by two sources within the same site. (`admin/` and `api/` are not reserved — the admin lives on its own port.)

## Cache and generated output

`.omniapp/cache.sqlite3`, `-wal`, and `-shm` files are generated. They contain normalized JSON records, an FTS5 index, and a reserved rebuildable embeddings table. They must not be used for backup or committed to source control. `_site/` is likewise generated and should be gitignored; `omniapp init` arranges both.

The cache is kept current by stat fingerprints: each cached record stores the `mtime` and size of its source files, and `validate`, `serve`, and `build` re-read only records whose fingerprints changed (a `touch` is enough to force one file's re-read). Changing anything under `.omniapp/models/` re-reads every record. A rewrite that preserves a file's mtime and size — effectively only a deliberate act — is the one edit fingerprints cannot see; `omniapp validate --full` re-reads every record from disk and rebuilds the cache, and deleting `cache.sqlite3` is always safe.
