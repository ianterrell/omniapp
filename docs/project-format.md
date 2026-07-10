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

Storage paths consist of literal directory segments and `{field}` segments. Every placeholder requires a field with a matching `path` source.

```yaml
version: 1
name: Scene
storage:
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

Supported field types are `string`, `text`, `integer`, `number`, `boolean`, `date`, `date_time`, `enum`, `reference`, `asset`, and `json`.

Sources:

- `path`: one directory segment captured from `storage.path`;
- `yaml`: a named key in a mapping; fields may share or use different YAML files;
- `markdown`: the complete UTF-8 contents of one file;
- `asset`: the project-relative path to a fixed-name file when that file exists.

Validation supports `min`, `max`, `min_length`, `max_length`, `pattern`, and `choices`. Dates use `YYYY-MM-DD`; date-times use RFC 3339.

Record identity is the string or integer `id` field when present, otherwise the record's project-relative directory. An explicit stable `id` is recommended when path fields are frequently renamed or other systems retain record URLs.

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

