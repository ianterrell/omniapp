//! End-to-end tests for omniapp-site over a temporary project fixture.

use std::fs;
use std::path::Path;

use omniapp_core::Workspace;
use omniapp_site::{BuildOptions, BuildReport, LoadedSite, Resolution, SiteError, build};
use tempfile::{TempDir, tempdir};

/// Unwrap a one-site build result into its report.
fn single_report(reports: Vec<(String, BuildReport)>) -> BuildReport {
    assert_eq!(reports.len(), 1, "expected exactly one site");
    reports.into_iter().next().unwrap().1
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A blog-shaped project exercising references, backrefs, generators, filters,
/// and asset copying.
fn blog() -> TempDir {
    let dir = tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        ".omniapp/config.yml",
        "version: 1\nname: Fixture Blog\n",
    );

    write(
        root,
        ".omniapp/models/author.yml",
        "version: 1\nname: Author\nstorage: { kind: file, path: \"authors/{id}.md\" }\nfields:\n  id: { type: string, source: { kind: path, variable: id } }\n  name: { type: string, source: { kind: frontmatter, key: name } }\n",
    );
    write(
        root,
        ".omniapp/models/tag.yml",
        "version: 1\nname: Tag\nstorage: { kind: file, path: \"tags/{slug}.md\" }\nfields:\n  slug: { type: string, source: { kind: path, variable: slug } }\n  name: { type: string, source: { kind: frontmatter, key: name } }\n",
    );
    write(
        root,
        ".omniapp/models/post.yml",
        "version: 1\nname: Post\nstorage: { kind: file, path: \"posts/{slug}.md\" }\nfields:\n  slug: { type: string, source: { kind: path, variable: slug } }\n  title: { type: string, source: { kind: frontmatter, key: title } }\n  date: { type: date, source: { kind: frontmatter, key: date } }\n  status: { type: string, source: { kind: frontmatter, key: status } }\n  author:\n    type: reference\n    reference: { model: Author, field: id }\n    source: { kind: frontmatter, key: author }\n  tags:\n    type: reference\n    reference: { model: Tag, field: slug, many: true }\n    source: { kind: frontmatter, key: tags }\n  body: { type: text, source: { kind: markdown } }\n",
    );
    write(
        root,
        ".omniapp/models/comment.yml",
        "version: 1\nname: Comment\nstorage: { kind: file, path: \"comments/{id}.md\" }\nfields:\n  id: { type: string, source: { kind: path, variable: id } }\n  post:\n    type: reference\n    reference: { model: Post, field: slug }\n    source: { kind: frontmatter, key: post }\n  body: { type: text, source: { kind: markdown } }\n",
    );
    write(
        root,
        ".omniapp/models/book.yml",
        "version: 1\nname: Book\nstorage: { kind: directory, path: \"books/{slug}\" }\nfields:\n  slug: { type: string, source: { kind: path, variable: slug } }\n  title: { type: string, source: { kind: yaml, file: book.yml, key: title } }\n  cover: { type: asset, source: { kind: asset, file: cover.jpg } }\n",
    );

    write(
        root,
        ".omniapp/views/published.yml",
        "version: 1\nname: published\nmodel: Post\ntype: table\nquery:\n  filters:\n    - { field: status, op: eq, value: published }\n  order:\n    - { field: date, direction: desc }\n",
    );

    // Records.
    write(root, "authors/ann.md", "---\nname: Ann Author\n---\n");
    write(root, "tags/rust.md", "---\nname: Rust\n---\n");
    write(
        root,
        "posts/hello.md",
        "---\ntitle: Hello World\ndate: 2026-01-02\nstatus: published\nauthor: ann\ntags:\n  - rust\n---\n# Heading\n\nSome **bold** text.\n",
    );
    write(
        root,
        "posts/draft.md",
        "---\ntitle: Draft Post\ndate: 2026-01-01\nstatus: draft\nauthor: ann\n---\nWork in progress.\n",
    );
    write(
        root,
        "comments/c1.md",
        "---\npost: hello\n---\nGreat post!\n",
    );
    write(root, "books/dune/book.yml", "title: Dune\n");
    write(root, "books/dune/cover.jpg", "JPEGBYTES");

    // Site sources.
    write(
        root,
        ".omniapp/sites/main/site.yml",
        "version: 1\ntitle: My Blog\nparams:\n  tagline: Thoughts and notes\n",
    );
    write(
        root,
        ".omniapp/sites/main/includes/nav.html",
        "<nav>{{ site.title }}: {{ site.params.tagline }}</nav>",
    );
    write(
        root,
        ".omniapp/sites/main/layouts/base.html",
        "<!doctype html><title>{{ page.title }} - {{ site.title }}</title><body>{% include \"includes/nav.html\" %}<main>{{ content }}</main></body>",
    );
    write(
        root,
        ".omniapp/sites/main/pages/index.html",
        "---\ntitle: Home\nlayout: base\n---\n<ul>{% for post in views.published %}<li>{{ post.title }} by {{ post.author.name }}</li>{% endfor %}</ul>\n<p>published-count:{{ records.Post | where(\"status\", \"published\") | length }}</p>\n<p>draft-count:{{ records.Post | where_not(\"status\", \"published\") | length }}</p>\n<p class=\"cover\">{{ \"books/dune/cover.jpg\" | asset_url }}</p>\n<p class=\"bad\">[{{ \"../secret\" | asset_url }}]</p>",
    );
    write(
        root,
        ".omniapp/sites/main/pages/about.md",
        "---\ntitle: About\nlayout: base\n---\n# About us\n\nThis is **markdown**.\n",
    );
    write(
        root,
        ".omniapp/sites/main/pages/post.html",
        "---\nlayout: base\nmodel: Post\npermalink: posts/{slug}/\n---\n<article><h1>{{ record.title }}</h1><p>By {{ record.author.name }}</p><p class=\"date\">{{ record.date | date(\"%B %Y\") }}</p><div class=\"body\">{{ record.body | markdown }}</div><ul>{% for tag in record.tags %}<li>{{ tag.name }}</li>{% endfor %}</ul><p class=\"url\">{{ record.url }}</p><div class=\"comments\">{% for c in record.inbound.Comment %}<p>{{ c.body }}</p>{% endfor %}</div></article>",
    );
    write(
        root,
        ".omniapp/sites/main/pages/published.html",
        "---\nview: published\npermalink: published/{slug}/\n---\n<h1>{{ record.title }}</h1>",
    );
    write(
        root,
        ".omniapp/sites/main/pages/404.html",
        "<h1>Missing</h1>",
    );

    dir
}

fn read(root: &Path, relative: &str) -> String {
    fs::read_to_string(root.join(relative)).unwrap_or_else(|_| panic!("missing {relative}"))
}

#[test]
fn builds_full_site_tree() {
    let dir = blog();
    let workspace = Workspace::new(dir.path());
    let out = dir.path().join("_site/main");
    let report = single_report(build(&workspace, &BuildOptions::default()).unwrap());

    assert!(
        report.errors.is_empty(),
        "unexpected errors: {:?}",
        report.errors
    );
    // index, about, 404.
    assert_eq!(report.pages, 3);
    // 2 posts + 1 published post.
    assert_eq!(report.record_pages, 3);
    assert_eq!(report.record_assets, 1);

    assert!(out.join("index.html").is_file());
    assert!(out.join("about/index.html").is_file());
    assert!(out.join("posts/hello/index.html").is_file());
    assert!(out.join("posts/draft/index.html").is_file());
    assert!(out.join("published/hello/index.html").is_file());
    assert!(
        !out.join("published/draft/index.html").exists(),
        "drafts must be excluded from the view generator"
    );
    assert!(out.join("404.html").is_file());
    assert!(out.join("files/books/dune/cover.jpg").is_file());
}

#[test]
fn markdown_layout_and_includes() {
    let dir = blog();
    let workspace = Workspace::new(dir.path());
    let out = dir.path().join("_site/main");
    build(&workspace, &BuildOptions::default()).unwrap();

    let about = read(&out, "about/index.html");
    assert!(about.contains("<h1>About us</h1>"), "{about}");
    assert!(about.contains("<strong>markdown</strong>"), "{about}");
    // Layout + include applied.
    assert!(
        about.contains("<nav>My Blog: Thoughts and notes</nav>"),
        "{about}"
    );
    assert!(about.contains("<title>About - My Blog</title>"), "{about}");
}

#[test]
fn references_backrefs_dates_and_urls() {
    let dir = blog();
    let workspace = Workspace::new(dir.path());
    let out = dir.path().join("_site/main");
    build(&workspace, &BuildOptions::default()).unwrap();

    let post = read(&out, "posts/hello/index.html");
    assert!(
        post.contains("By Ann Author"),
        "reference resolution: {post}"
    );
    assert!(
        post.contains("<li>Rust</li>"),
        "many-reference resolution: {post}"
    );
    assert!(
        post.contains(r#"<p class="date">January 2026</p>"#),
        "date filter: {post}"
    );
    assert!(
        post.contains("<strong>bold</strong>"),
        "markdown filter: {post}"
    );
    // URLs are percent-encoded and marked safe, so slashes stay literal.
    assert!(
        post.contains(r#"<p class="url">/posts/hello/</p>"#),
        "record.url: {post}"
    );
    assert!(post.contains("Great post!"), "inbound backref: {post}");
}

#[test]
fn index_filters_and_asset_urls() {
    let dir = blog();
    let workspace = Workspace::new(dir.path());
    let out = dir.path().join("_site/main");
    build(&workspace, &BuildOptions::default()).unwrap();

    let index = read(&out, "index.html");
    assert!(index.contains("Hello World by Ann Author"), "{index}");
    assert!(
        !index.contains("Draft Post"),
        "view excludes drafts: {index}"
    );
    assert!(index.contains("published-count:1"), "where filter: {index}");
    assert!(index.contains("draft-count:1"), "where_not filter: {index}");
    assert!(
        index.contains(r#"<p class="cover">/files/books/dune/cover.jpg</p>"#),
        "asset_url: {index}"
    );
    assert!(
        index.contains(r#"<p class="bad">[]</p>"#),
        "asset_url rejects ../: {index}"
    );
}

#[test]
fn resolve_exact_redirect_and_not_found() {
    let dir = blog();
    let workspace = Workspace::new(dir.path());
    let site = LoadedSite::load(&workspace, "main").unwrap();
    assert!(site.has_site());

    match site.resolve("/").unwrap() {
        Resolution::Html(html) => assert!(html.contains("<nav>My Blog")),
        other => panic!("expected html, got {other:?}"),
    }
    match site.resolve("/about").unwrap() {
        Resolution::Redirect(target) => assert_eq!(target, "/about/"),
        other => panic!("expected redirect, got {other:?}"),
    }
    match site.resolve("/posts/hello/").unwrap() {
        Resolution::Html(html) => assert!(html.contains("By Ann Author")),
        other => panic!("expected html, got {other:?}"),
    }
    match site.resolve("/does-not-exist").unwrap() {
        Resolution::NotFound { html } => {
            assert!(html.unwrap().contains("<h1>Missing</h1>"));
        }
        other => panic!("expected not found, got {other:?}"),
    }
}

#[test]
fn missing_site_has_no_site() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".omniapp/config.yml",
        "version: 1\nname: Bare\n",
    );
    let workspace = Workspace::new(dir.path());
    let site = LoadedSite::load(&workspace, "main").unwrap();
    assert!(!site.has_site());
    match site.resolve("/anything").unwrap() {
        Resolution::NotFound { html } => assert!(html.is_none()),
        other => panic!("expected empty not found, got {other:?}"),
    }
}

#[test]
fn detects_url_collisions() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".omniapp/config.yml",
        "version: 1\nname: Collide\n",
    );
    write(
        dir.path(),
        ".omniapp/sites/main/pages/a.html",
        "---\npermalink: same/\n---\nA",
    );
    write(
        dir.path(),
        ".omniapp/sites/main/pages/b.html",
        "---\npermalink: same/\n---\nB",
    );
    let workspace = Workspace::new(dir.path());
    match LoadedSite::load(&workspace, "main") {
        Err(SiteError::Collision { url, .. }) => assert_eq!(url, "/same/"),
        Err(other) => panic!("expected collision, got {other:?}"),
        Ok(_) => panic!("expected collision error"),
    }
}

#[test]
fn rejects_reserved_prefixes() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".omniapp/config.yml",
        "version: 1\nname: Reserved\n",
    );
    write(
        dir.path(),
        ".omniapp/sites/main/pages/assets/x.html",
        "hello",
    );
    let workspace = Workspace::new(dir.path());
    match LoadedSite::load(&workspace, "main") {
        Err(SiteError::Reserved { url, .. }) => assert_eq!(url, "/assets/x/"),
        Err(other) => panic!("expected reserved error, got {other:?}"),
        Ok(_) => panic!("expected reserved error"),
    }
}

#[test]
fn render_errors_leave_existing_output_untouched() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".omniapp/config.yml",
        "version: 1\nname: Broken\n",
    );
    // A template that raises at render time (calling a value as a function).
    write(
        dir.path(),
        ".omniapp/sites/main/pages/index.html",
        "{{ 1 + \"x\" }}",
    );
    let workspace = Workspace::new(dir.path());
    let out = dir.path().join("_site/main");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("sentinel.txt"), "keep me").unwrap();

    let report = single_report(build(&workspace, &BuildOptions::default()).unwrap());
    assert!(!report.errors.is_empty());
    assert_eq!(
        fs::read_to_string(out.join("sentinel.txt")).unwrap(),
        "keep me"
    );
    assert!(!out.join("index.html").exists());
    assert!(
        !out.with_file_name("main.new").exists(),
        "staging must be cleaned up"
    );
}

#[test]
fn strict_mode_flags_undefined_lookups() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".omniapp/config.yml",
        "version: 1\nname: Strict\n",
    );
    write(
        dir.path(),
        ".omniapp/sites/main/pages/index.html",
        "value: {{ ghost.value }}",
    );
    let workspace = Workspace::new(dir.path());

    let lenient = single_report(build(&workspace, &BuildOptions::default()).unwrap());
    assert!(lenient.errors.is_empty(), "{:?}", lenient.errors);

    let strict = single_report(
        build(
            &workspace,
            &BuildOptions {
                strict: true,
                ..BuildOptions::default()
            },
        )
        .unwrap(),
    );
    assert!(
        !strict.errors.is_empty(),
        "strict mode should reject undefined"
    );
}

#[test]
fn multiple_sites_build_independently() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(root, ".omniapp/config.yml", "version: 1\nname: Multi\n");
    write(
        root,
        ".omniapp/models/post.yml",
        "version: 1\nname: Post\nstorage: { kind: file, path: \"posts/{slug}.md\" }\nfields:\n  slug: { type: string, source: { kind: path, variable: slug } }\n  title: { type: string, source: { kind: frontmatter, key: title } }\n",
    );
    write(root, "posts/hello.md", "---\ntitle: Hello\n---\nBody\n");

    // Two sites over the same records, publishing the same model at
    // different permalinks. `admin/` pages are legal now that the admin
    // lives on its own port.
    write(
        root,
        ".omniapp/sites/blog/pages/index.html",
        "blog: {% for p in records.Post %}{{ p.url }}{% endfor %}",
    );
    write(
        root,
        ".omniapp/sites/blog/pages/post.html",
        "---\nmodel: Post\npermalink: posts/{slug}/\n---\n<h1>{{ record.title }}</h1>",
    );
    write(
        root,
        ".omniapp/sites/blog/pages/admin/notes.html",
        "not reserved",
    );
    write(
        root,
        ".omniapp/sites/mirror/pages/index.html",
        "mirror: {% for p in records.Post %}{{ p.url }}{% endfor %}",
    );
    write(
        root,
        ".omniapp/sites/mirror/pages/post.html",
        "---\nmodel: Post\npermalink: writing/{slug}/\n---\n<h2>{{ record.title }}</h2>",
    );

    let workspace = Workspace::new(root);
    let reports = build(&workspace, &BuildOptions::default()).unwrap();
    let names = reports
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["blog", "mirror"]);
    for (_, report) in &reports {
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    let blog_index = read(&root.join("_site/blog"), "index.html");
    assert!(blog_index.contains("/posts/hello/"), "{blog_index}");
    let mirror_index = read(&root.join("_site/mirror"), "index.html");
    assert!(mirror_index.contains("/writing/hello/"), "{mirror_index}");
    assert!(root.join("_site/blog/posts/hello/index.html").is_file());
    assert!(root.join("_site/blog/admin/notes/index.html").is_file());
    assert!(root.join("_site/mirror/writing/hello/index.html").is_file());

    // --site builds only the named site.
    fs::remove_dir_all(root.join("_site")).unwrap();
    let only = build(
        &workspace,
        &BuildOptions {
            site: Some("mirror".into()),
            ..BuildOptions::default()
        },
    )
    .unwrap();
    assert_eq!(only.len(), 1);
    assert!(root.join("_site/mirror/index.html").is_file());
    assert!(!root.join("_site/blog").exists());
}
