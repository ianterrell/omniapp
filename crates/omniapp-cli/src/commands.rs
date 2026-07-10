use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use omniapp_core::{Cache, Page, Record, RecordInput, SearchHit, Workspace, execute_query};
use omniapp_schema::{FieldType, Model, Query};
use serde::Serialize;
use serde_json::{Value, json};

pub fn list(
    path: &Path,
    model_name: &str,
    page: usize,
    page_size: usize,
    json: bool,
) -> Result<()> {
    ensure_page_size(page_size)?;
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let model = loaded
        .models
        .get(model_name)
        .with_context(|| format!("unknown model {model_name:?}"))?;
    let records = workspace.records(model)?;
    let query = Query {
        page_size,
        ..Query::default()
    };
    let result = execute_query(&records, &query, page);
    if json {
        print_json(&result)
    } else {
        let fields = list_fields(model);
        print_record_table(model, &fields, &result);
        print_page_summary(&result);
        Ok(())
    }
}

pub fn get(path: &Path, model_name: &str, selector: &str, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let model = loaded
        .models
        .get(model_name)
        .with_context(|| format!("unknown model {model_name:?}"))?;
    let records = workspace.records(model)?;
    let record = resolve_record(&records, model_name, selector)?;
    if json {
        print_json(record)
    } else {
        print_record(model, record);
        Ok(())
    }
}

pub fn create(
    path: &Path,
    model_name: &str,
    sets: &[String],
    input: Option<&Path>,
    json: bool,
) -> Result<()> {
    let values = read_values(sets, input)?;
    let workspace = Workspace::new(path);
    let record = workspace.save_record(
        model_name,
        None,
        RecordInput {
            revision: None,
            values,
        },
    )?;
    if json {
        print_json(&record)
    } else {
        println!(
            "Created {} {:?} at {}",
            record.model,
            record.key,
            record.path.display()
        );
        Ok(())
    }
}

pub fn update(
    path: &Path,
    model_name: &str,
    selector: &str,
    sets: &[String],
    input: Option<&Path>,
    json: bool,
) -> Result<()> {
    let values = read_values(sets, input)?;
    if values.is_empty() {
        bail!("no updates supplied; use --set or --input");
    }
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let model = loaded
        .models
        .get(model_name)
        .with_context(|| format!("unknown model {model_name:?}"))?;
    let records = workspace.records(model)?;
    let existing = resolve_record(&records, model_name, selector)?;
    let key = existing.key.clone();
    let record = workspace.save_record(
        model_name,
        Some(&key),
        RecordInput {
            revision: Some(existing.revision.clone()),
            values,
        },
    )?;
    if json {
        print_json(&record)
    } else {
        println!(
            "Updated {} {:?} at {}",
            record.model,
            record.key,
            record.path.display()
        );
        Ok(())
    }
}

pub fn delete(path: &Path, model_name: &str, selector: &str, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let model = loaded
        .models
        .get(model_name)
        .with_context(|| format!("unknown model {model_name:?}"))?;
    let records = workspace.records(model)?;
    let record = resolve_record(&records, model_name, selector)?;
    let key = record.key.clone();
    let revision = record.revision.clone();
    let record_path = record.path.clone();
    workspace.delete_record(model_name, &key, Some(&revision))?;
    if json {
        print_json(&json!({
            "deleted": true,
            "model": model_name,
            "key": key,
            "path": record_path,
        }))
    } else {
        println!("Deleted {model_name} {key:?} at {}", record_path.display());
        Ok(())
    }
}

pub fn query(
    path: &Path,
    view_name: &str,
    page: usize,
    page_size: Option<usize>,
    json: bool,
) -> Result<()> {
    if let Some(page_size) = page_size {
        ensure_page_size(page_size)?;
    }
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let view = loaded
        .views
        .get(view_name)
        .with_context(|| format!("unknown view {view_name:?}"))?;
    let model = loaded
        .models
        .get(&view.model)
        .with_context(|| format!("view references unknown model {:?}", view.model))?;
    let records = workspace.records(model)?;
    let mut record_query = view.query.clone();
    if let Some(page_size) = page_size {
        record_query.page_size = page_size;
    }
    let result = execute_query(&records, &record_query, page);
    if json {
        print_json(&result)
    } else {
        let fields = if view.fields.is_empty() {
            list_fields(model)
        } else {
            view.fields.clone()
        };
        println!("{}", view.label.as_deref().unwrap_or(&view.name));
        print_record_table(model, &fields, &result);
        print_page_summary(&result);
        Ok(())
    }
}

pub fn search(path: &Path, search_query: &str, limit: usize, json: bool) -> Result<()> {
    if limit == 0 || limit > 10_000 {
        bail!("--limit must be between 1 and 10000");
    }
    let workspace = Workspace::new(path);
    workspace.rebuild_cache()?;
    let cache = Cache::open(&workspace.metadata_dir().join("cache.sqlite3"))?;
    let hits = cache.search(search_query, limit)?;
    if json {
        print_json(&hits)
    } else {
        print_search_hits(&hits);
        println!("{} result(s)", hits.len());
        Ok(())
    }
}

pub fn relationships(path: &Path, model_name: &str, selector: &str, json: bool) -> Result<()> {
    let workspace = Workspace::new(path);
    let loaded = workspace.load()?;
    let model = loaded
        .models
        .get(model_name)
        .with_context(|| format!("unknown model {model_name:?}"))?;
    let records = workspace.records(model)?;
    let key = resolve_record(&records, model_name, selector)?.key.clone();
    let relationships = workspace.relationships(model_name, &key)?;
    if json {
        return print_json(&relationships);
    }
    println!(
        "{} {:?}",
        relationships.record.model, relationships.record.key
    );
    println!("Outbound relationships:");
    if relationships.outbound.is_empty() {
        println!("  None");
    } else {
        for link in relationships.outbound {
            println!(
                "  {} -> {} {:?} ({})",
                link.field,
                link.record.model,
                link.record.key,
                link.record.path.display()
            );
        }
    }
    println!("Inbound backreferences:");
    if relationships.inbound.is_empty() {
        println!("  None");
    } else {
        for link in relationships.inbound {
            println!(
                "  {} {:?} via {} ({})",
                link.record.model,
                link.record.key,
                link.field,
                link.record.path.display()
            );
        }
    }
    Ok(())
}

fn read_values(sets: &[String], input: Option<&Path>) -> Result<BTreeMap<String, Value>> {
    let mut values = if let Some(input) = input {
        let contents = if input == Path::new("-") {
            let mut contents = String::new();
            io::stdin().read_to_string(&mut contents)?;
            contents
        } else {
            fs::read_to_string(input)
                .with_context(|| format!("could not read input file {}", input.display()))?
        };
        parse_input_object(&contents)?
    } else {
        BTreeMap::new()
    };

    for assignment in sets {
        let (field, raw_value) = assignment
            .split_once('=')
            .with_context(|| format!("invalid --set {assignment:?}; expected FIELD=VALUE"))?;
        if field.trim().is_empty() {
            bail!("invalid --set {assignment:?}; field name must not be empty");
        }
        let value =
            serde_json::from_str(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        values.insert(field.to_owned(), value);
    }
    Ok(values)
}

fn parse_input_object(contents: &str) -> Result<BTreeMap<String, Value>> {
    let value: Value = serde_json::from_str(contents).context("input must be valid JSON")?;
    let object = value
        .as_object()
        .context("input must be a JSON object containing field values")?;
    let object = if object.len() == 1 {
        object
            .get("values")
            .and_then(Value::as_object)
            .unwrap_or(object)
    } else {
        object
    };
    Ok(object
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect())
}

fn resolve_record<'a>(
    records: &'a [Record],
    model_name: &str,
    selector: &str,
) -> Result<&'a Record> {
    if let Some(record) = records
        .iter()
        .find(|record| record.key == selector || record.path.to_string_lossy() == selector)
    {
        return Ok(record);
    }
    let matches = records
        .iter()
        .filter(|record| {
            ["id", "slug"].iter().any(|field| {
                record
                    .values
                    .get(*field)
                    .is_some_and(|value| scalar_text(value).as_deref() == Some(selector))
            })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [record] => Ok(record),
        [] => bail!("unknown {model_name} record {selector:?}"),
        _ => bail!(
            "record selector {selector:?} is ambiguous; use one of: {}",
            matches
                .iter()
                .map(|record| record.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn list_fields(model: &Model) -> Vec<String> {
    model
        .fields
        .iter()
        .filter(|(_, field)| !matches!(field.field_type, FieldType::Text | FieldType::Json))
        .map(|(name, _)| name.clone())
        .collect()
}

fn print_record_table(model: &Model, fields: &[String], page: &Page) {
    let mut headers = vec!["KEY".to_owned()];
    headers.extend(fields.iter().map(|name| {
        model
            .fields
            .get(name)
            .and_then(|field| field.label.clone())
            .unwrap_or_else(|| name.clone())
    }));
    let rows = page
        .records
        .iter()
        .map(|record| {
            let mut row = vec![record.key.clone()];
            row.extend(fields.iter().map(|field| {
                record
                    .values
                    .get(field)
                    .map_or_else(|| "—".to_owned(), display_value)
            }));
            row
        })
        .collect::<Vec<_>>();
    print_table(&headers, &rows);
}

fn print_record(model: &Model, record: &Record) {
    println!("{} {:?}", record.model, record.key);
    println!("Path: {}", record.path.display());
    for (name, field) in &model.fields {
        let label = field.label.as_deref().unwrap_or(name);
        let value = record
            .values
            .get(name)
            .map_or("—".to_owned(), display_value);
        if value.contains('\n') {
            println!("{label}:");
            for line in value.lines() {
                println!("  {line}");
            }
        } else {
            println!("{label}: {value}");
        }
    }
}

fn print_search_hits(hits: &[SearchHit]) {
    let headers = ["MODEL".to_owned(), "KEY".to_owned(), "PATH".to_owned()];
    let rows = hits
        .iter()
        .map(|hit| vec![hit.model.clone(), hit.key.clone(), hit.path.clone()])
        .collect::<Vec<_>>();
    print_table(&headers, &rows);
}

fn print_table(headers: &[String], rows: &[Vec<String>]) {
    if rows.is_empty() {
        println!("No records.");
        return;
    }
    let widths = headers
        .iter()
        .enumerate()
        .map(|(column, header)| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|value| value.chars().count())
                .chain(std::iter::once(header.chars().count()))
                .max()
                .unwrap_or(0)
                .min(40)
        })
        .collect::<Vec<_>>();
    print_table_row(headers, &widths);
    println!(
        "{}",
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for row in rows {
        print_table_row(row, &widths);
    }
}

fn print_table_row(values: &[String], widths: &[usize]) {
    println!(
        "{}",
        values
            .iter()
            .zip(widths)
            .map(|(value, width)| format!("{:<width$}", truncate(value, *width), width = width))
            .collect::<Vec<_>>()
            .join("  ")
    );
}

fn print_page_summary(page: &Page) {
    println!(
        "Page {} of {} · {} total record(s)",
        page.page,
        page.pages.max(1),
        page.total
    );
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "—".to_owned(),
        Value::String(value) => value.replace('\n', " ↵ "),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| "{…}".to_owned()),
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    value
        .chars()
        .take(width - 1)
        .chain(std::iter::once('…'))
        .collect()
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn ensure_page_size(page_size: usize) -> Result<()> {
    if page_size == 0 || page_size > 1000 {
        bail!("--page-size must be between 1 and 1000");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn set_values_decode_json_and_fall_back_to_strings() {
        let values = read_values(
            &["title=Dune".into(), "rating=5".into(), "done=true".into()],
            None,
        )
        .unwrap();
        assert_eq!(values["title"], "Dune");
        assert_eq!(values["rating"], 5);
        assert_eq!(values["done"], true);
    }

    #[test]
    fn input_accepts_plain_and_api_envelope_objects() {
        assert_eq!(
            parse_input_object(r#"{"title":"Dune"}"#).unwrap()["title"],
            "Dune"
        );
        assert_eq!(
            parse_input_object(r#"{"values":{"title":"Dune"}}"#).unwrap()["title"],
            "Dune"
        );
    }

    #[test]
    fn selector_accepts_slug() {
        let records = [Record {
            key: "books/dune".into(),
            model: "Book".into(),
            path: PathBuf::from("books/dune"),
            revision: "test".into(),
            values: BTreeMap::from([("slug".into(), json!("dune"))]),
        }];
        assert_eq!(
            resolve_record(&records, "Book", "dune").unwrap().key,
            "books/dune"
        );
    }
}
