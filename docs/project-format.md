# Project format version 1

All paths below are relative to the project root.

## Configuration

`.omniapp/config.yml`:

```yaml
version: 1
name: Editorial calendar
description: Local publishing workflow
```

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

Validation supports `min`, `max`, `min_length`, `max_length`, `pattern`, and `choices`. Dates use `YYYY-MM-DD`; date-times use RFC 3339.

Reference fields resolve in both directions. For example, a `Scene.book` reference to `Book.slug` appears as an outbound relationship on the scene and an inbound backreference on the book. Values used as relationship targets must be unique within their model. Use `omniapp relationships Scene opening --json` or the record relationships HTTP endpoint to traverse the graph.

Record identity is the string or integer `id` field when present, otherwise the project-relative record file or directory. An explicit stable `id` is recommended when path fields are frequently renamed or other systems retain record URLs.

Every record returned by the core, CLI JSON output, or HTTP API also has a `revision`. Update bodies include that revision, and delete requests pass it as a query parameter. A revision mismatch means project files changed after the caller read them; OmniApp rejects the mutation instead of overwriting those changes.

Named `outputs` are safe project-relative templates. Placeholders must name model fields and may appear in filenames. `omniapp outputs Book dune --json` returns every resolved path plus whether it currently exists as a file or directory; the generated bytes remain ordinary filesystem artifacts rather than record data.

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
actions:
  - name: publish
    label: Publish now
    script: publish-post
```

Filter operators are `eq`, `not_eq`, `lt`, `lte`, `gt`, `gte`, `contains`, `in`, `is_null`, and `is_not_null`. Null operators omit `value`; all others require it. Page sizes must be between 1 and 1000.

Recognized view types are `form`, `table`, `tree`, `board`, `calendar`, `gallery`, `timeline`, and `custom`. Version 1 of the browser client renders the common table/form surface; definitions using the other types remain valid for future specialized renderers.

## Cache

`.omniapp/cache.sqlite3`, `-wal`, and `-shm` files are generated. They contain normalized JSON records, an FTS5 index, and a reserved rebuildable embeddings table. They must not be used for backup or committed to source control.
