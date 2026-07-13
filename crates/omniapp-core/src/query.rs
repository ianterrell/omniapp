use std::cmp::Ordering;

use omniapp_schema::{Direction, Filter, FilterOp, Query};
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

#[must_use]
pub fn execute_query(records: &[Record], query: &Query, page: usize) -> Page {
    let page = page.max(1);
    let page_size = query.page_size.clamp(1, 1000);
    let matches = execute_query_all(records, query);
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
    let mut matches: Vec<Record> = records
        .iter()
        .filter(|record| {
            query
                .filters
                .iter()
                .all(|filter| matches_filter(record, filter))
        })
        .cloned()
        .collect();

    matches.sort_by(|left, right| {
        for order in &query.order {
            let ordering = compare_values(
                left.values.get(&order.field),
                right.values.get(&order.field),
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

fn matches_filter(record: &Record, filter: &Filter) -> bool {
    let actual = record
        .values
        .get(&filter.field)
        .filter(|value| !value.is_null());
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

    fn record(key: &str, values: Value) -> Record {
        Record {
            key: key.into(),
            model: "Post".into(),
            path: PathBuf::new(),
            revision: "test".into(),
            values: serde_json::from_value::<BTreeMap<String, Value>>(values).unwrap(),
        }
    }
}
