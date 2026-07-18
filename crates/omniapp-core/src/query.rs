use std::cmp::Ordering;
use std::collections::BTreeMap;

use omniapp_schema::{Direction, Filter, FilterOp, Model, Query};
use serde::Serialize;
use serde_json::Value;

use crate::Record;

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub records: Vec<Record>,
    pub page: usize,
    pub page_size: usize,
    pub total: usize,
    pub pages: usize,
}

/// One group of a grouped list view: the resolved group value, the records
/// kept for display (after any per-group limit), and the group's full size.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub value: Option<Value>,
    pub records: Vec<Record>,
    pub total: usize,
}

/// A grouped list result: ordered groups plus the overall matching total.
#[derive(Debug, Clone, Serialize)]
pub struct GroupedPage {
    pub groups: Vec<Group>,
    pub total: usize,
    pub grouped: bool,
}

#[must_use]
pub fn execute_query(records: &[Record], query: &Query, page: usize) -> Page {
    execute_query_with_relations(records, records, &BTreeMap::new(), query, page)
}

/// Apply a query while allowing dotted fields to traverse single-valued
/// references, for example `book.publication_state`.
#[must_use]
pub fn execute_query_with_relations(
    records: &[Record],
    all_records: &[Record],
    models: &BTreeMap<String, Model>,
    query: &Query,
    page: usize,
) -> Page {
    let page = page.max(1);
    let page_size = query.page_size.clamp(1, 1000);
    let matches = execute_query_all_with_relations(records, all_records, models, query);
    let total = matches.len();
    let pages = total.div_ceil(page_size);
    let start = (page - 1).saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    Page {
        records: matches[start..end].to_vec(),
        page,
        page_size,
        total,
        pages,
    }
}

/// Apply a query's filters and order without pagination.
#[must_use]
pub fn execute_query_all(records: &[Record], query: &Query) -> Vec<Record> {
    execute_query_all_with_relations(records, records, &BTreeMap::new(), query)
}

/// Apply a query without pagination, resolving dotted fields through
/// single-valued references declared by the models.
#[must_use]
pub fn execute_query_all_with_relations(
    records: &[Record],
    all_records: &[Record],
    models: &BTreeMap<String, Model>,
    query: &Query,
) -> Vec<Record> {
    let mut matches: Vec<Record> = records
        .iter()
        .filter(|record| {
            query
                .filters
                .iter()
                .all(|filter| matches_filter(record, filter, all_records, models))
        })
        .cloned()
        .collect();

    matches.sort_by(|left, right| {
        for order in &query.order {
            let ordering = compare_values(
                resolve_value(left, &order.field, all_records, models),
                resolve_value(right, &order.field, all_records, models),
            );
            let ordering = match order.direction {
                Direction::Asc => ordering,
                Direction::Desc => ordering.reverse(),
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        left.key.cmp(&right.key)
    });
    matches
}

fn matches_filter(
    record: &Record,
    filter: &Filter,
    all_records: &[Record],
    models: &BTreeMap<String, Model>,
) -> bool {
    let actual =
        resolve_value(record, &filter.field, all_records, models).filter(|value| !value.is_null());
    match filter.op {
        FilterOp::IsNull => actual.is_none(),
        FilterOp::IsNotNull => actual.is_some(),
        FilterOp::Eq => actual == filter.value.as_ref(),
        FilterOp::NotEq => actual != filter.value.as_ref(),
        FilterOp::Lt => compare_values(actual, filter.value.as_ref()) == Ordering::Less,
        FilterOp::Lte => compare_values(actual, filter.value.as_ref()) != Ordering::Greater,
        FilterOp::Gt => compare_values(actual, filter.value.as_ref()) == Ordering::Greater,
        FilterOp::Gte => compare_values(actual, filter.value.as_ref()) != Ordering::Less,
        FilterOp::Contains => match (actual, filter.value.as_ref()) {
            (Some(Value::String(actual)), Some(Value::String(needle))) => actual.contains(needle),
            (Some(Value::Array(actual)), Some(needle)) => actual.contains(needle),
            _ => false,
        },
        FilterOp::In => match (actual, filter.value.as_ref()) {
            (Some(actual), Some(Value::Array(options))) => options.contains(actual),
            _ => false,
        },
    }
}

fn resolve_value<'a>(
    record: &'a Record,
    field_path: &str,
    all_records: &'a [Record],
    models: &BTreeMap<String, Model>,
) -> Option<&'a Value> {
    let mut current = record;
    let mut segments = field_path.split('.').peekable();
    while let Some(segment) = segments.next() {
        let value = current.values.get(segment)?;
        if segments.peek().is_none() {
            return Some(value);
        }
        let field = models.get(&current.model)?.fields.get(segment)?;
        let reference = field
            .reference
            .as_ref()
            .filter(|reference| !reference.many)?;
        current = all_records.iter().find(|candidate| {
            candidate.model == reference.model
                && candidate.values.get(&reference.field) == Some(value)
        })?;
    }
    None
}

fn compare_values(left: Option<&Value>, right: Option<&Value>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(Value::Number(left)), Some(Value::Number(right))) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(Ordering::Equal),
        (Some(Value::String(left)), Some(Value::String(right))) => left.cmp(right),
        (Some(Value::Bool(left)), Some(Value::Bool(right))) => left.cmp(right),
        (Some(left), Some(right)) => left.to_string().cmp(&right.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use omniapp_schema::{Direction, Filter, FilterOp, Order};
    use serde_json::json;

    use super::*;

    #[test]
    fn filters_sorts_and_paginates() {
        let records = [
            record(
                "a",
                json!({"status": "scheduled", "date": "2026-02-02", "posted": null}),
            ),
            record(
                "b",
                json!({"status": "draft", "date": "2026-01-01", "posted": null}),
            ),
            record(
                "c",
                json!({"status": "scheduled", "date": "2026-01-02", "posted": null}),
            ),
        ];
        let query = Query {
            filters: vec![
                Filter {
                    field: "status".into(),
                    op: FilterOp::Eq,
                    value: Some(json!("scheduled")),
                },
                Filter {
                    field: "posted".into(),
                    op: FilterOp::IsNull,
                    value: None,
                },
            ],
            order: vec![Order {
                field: "date".into(),
                direction: Direction::Asc,
            }],
            page_size: 1,
        };
        let first = execute_query(&records, &query, 1);
        let second = execute_query(&records, &query, 2);
        assert_eq!(first.total, 2);
        assert_eq!(first.records[0].key, "c");
        assert_eq!(second.records[0].key, "a");
    }

    #[test]
    fn filters_through_single_valued_references() {
        let todo_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Todo
storage: { kind: file, path: "todos/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
  done: { type: boolean, source: { kind: frontmatter, key: done } }
  book:
    type: reference
    source: { kind: frontmatter, key: book }
    reference: { model: Book, field: slug }
"#,
        )
        .unwrap();
        let book_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Book
storage: { kind: directory, path: "books/{slug}" }
fields:
  slug: { type: string, source: { kind: path, variable: slug } }
  publication_state: { type: string, source: { kind: yaml, file: book.yml, key: publication_state } }
"#,
        )
        .unwrap();
        let models = BTreeMap::from([
            ("Todo".to_owned(), todo_model),
            ("Book".to_owned(), book_model),
        ]);
        let todos = vec![
            typed_record("active", "Todo", json!({"done": false, "book": "active"})),
            typed_record(
                "archived",
                "Todo",
                json!({"done": false, "book": "archived"}),
            ),
            typed_record("general", "Todo", json!({"done": false})),
        ];
        let mut all_records = todos.clone();
        all_records.extend([
            typed_record(
                "active",
                "Book",
                json!({"slug": "active", "publication_state": "draft"}),
            ),
            typed_record(
                "archived",
                "Book",
                json!({"slug": "archived", "publication_state": "archived"}),
            ),
        ]);
        let query = Query {
            filters: vec![Filter {
                field: "book.publication_state".into(),
                op: FilterOp::NotEq,
                value: Some(json!("archived")),
            }],
            order: vec![],
            page_size: 50,
        };
        let page = execute_query_with_relations(&todos, &all_records, &models, &query, 1);
        assert_eq!(
            page.records
                .iter()
                .map(|record| record.key.as_str())
                .collect::<Vec<_>>(),
            vec!["active", "general"]
        );
    }

    fn record(key: &str, values: Value) -> Record {
        typed_record(key, "Post", values)
    }

    fn typed_record(key: &str, model: &str, values: Value) -> Record {
        Record {
            key: key.into(),
            model: model.into(),
            path: PathBuf::new(),
            revision: "test".into(),
            values: serde_json::from_value::<BTreeMap<String, Value>>(values).unwrap(),
        }
    }
}
