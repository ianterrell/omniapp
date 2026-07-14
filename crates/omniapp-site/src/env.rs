//! The minijinja [`Environment`]: loader wiring and custom filters.

use std::path::{Path, PathBuf};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, NaiveDate};
use minijinja::value::Value;
use minijinja::{AutoEscape, Environment, Error, UndefinedBehavior, path_loader};
use omniapp_schema::is_safe_relative;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use pulldown_cmark::{Options, Parser};

/// Characters kept literal inside a single URL path segment.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Build the template environment for a site. Layouts and includes are served
/// from the site directory through the path loader; page bodies are added by
/// the render step. `strict` switches undefined lookups from chainable to
/// strict, surfacing typos in templates.
pub(crate) fn build_environment(site_dir: &Path, strict: bool) -> Environment<'static> {
    let mut env = Environment::new();
    // The loader owns its path so the environment is `'static`.
    env.set_loader(path_loader(PathBuf::from(site_dir)));
    env.set_undefined_behavior(if strict {
        UndefinedBehavior::Strict
    } else {
        UndefinedBehavior::Chainable
    });
    env.set_auto_escape_callback(|name| {
        let extension = name.rsplit('.').next();
        if matches!(extension, Some("html" | "htm" | "xml")) {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.add_filter("markdown", markdown_filter);
    env.add_filter("asset_url", asset_url_filter);
    env.add_filter("date", date_filter);
    env.add_filter("where", where_filter);
    env.add_filter("where_not", where_not_filter);
    env
}

/// Render CommonMark source to HTML with tables, strikethrough, and footnotes.
/// Public so the admin's markdown fields render exactly like the site's.
#[must_use]
pub fn render_markdown(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(source, options);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

fn markdown_filter(input: &str) -> Value {
    Value::from_safe_string(render_markdown(input))
}

/// Percent-encode every segment of a URL path, leaving `/` separators intact.
/// The result contains no characters HTML escaping would rewrite, so it is
/// safe to emit unescaped.
pub(crate) fn encode_url_path(path: &str) -> String {
    path.split('/')
        .map(|segment| utf8_percent_encode(segment, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn asset_url_filter(input: Option<String>) -> Value {
    let Some(path) = input.filter(|path| !path.is_empty()) else {
        return Value::from("");
    };
    if !is_safe_relative(&path) {
        return Value::from("");
    }
    Value::from_safe_string(format!("/files/{}", encode_url_path(&path)))
}

fn date_filter(input: String, format: &str) -> String {
    // A malformed strftime string panics at format time, so reject it up front.
    let items: Vec<Item<'_>> = StrftimeItems::new(format).collect();
    if items.iter().any(|item| matches!(item, Item::Error)) {
        return input;
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(&input) {
        return datetime.format_with_items(items.iter()).to_string();
    }
    if let Ok(date) = NaiveDate::parse_from_str(&input, "%Y-%m-%d") {
        return date.format_with_items(items.iter()).to_string();
    }
    input
}

fn where_filter(list: &Value, attr: &str, value: &Value) -> Result<Value, Error> {
    filter_list(list, attr, value, true)
}

fn where_not_filter(list: &Value, attr: &str, value: &Value) -> Result<Value, Error> {
    filter_list(list, attr, value, false)
}

fn filter_list(list: &Value, attr: &str, value: &Value, keep_equal: bool) -> Result<Value, Error> {
    let mut kept = Vec::new();
    for item in list.try_iter()? {
        let actual = item.get_attr(attr)?;
        if (actual == *value) == keep_equal {
            kept.push(item);
        }
    }
    Ok(Value::from(kept))
}
