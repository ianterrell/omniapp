use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omniapp_schema::{Direction, FilterOp, Query};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row, params, params_from_iter};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{Page, Record};

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("could not serialize record for cache: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub struct Cache {
    connection: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub model: String,
    pub key: String,
    pub path: String,
    pub rank: f64,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS cache_metadata (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS records (
               model TEXT NOT NULL,
               record_key TEXT NOT NULL,
               path TEXT NOT NULL,
               data TEXT NOT NULL,
               PRIMARY KEY(model, record_key)
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(
               model UNINDEXED,
               record_key UNINDEXED,
               path UNINDEXED,
               content
             );
             CREATE TABLE IF NOT EXISTS vector_embeddings (
               model TEXT NOT NULL,
               record_key TEXT NOT NULL,
               field TEXT NOT NULL,
               dimensions INTEGER NOT NULL,
               embedding BLOB NOT NULL,
               PRIMARY KEY(model, record_key, field)
             );",
        )?;
        Ok(Self { connection })
    }

    pub fn rebuild(&mut self, records: &[Record]) -> Result<(), CacheError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM records", [])?;
        transaction.execute("DELETE FROM records_fts", [])?;
        transaction.execute("DELETE FROM vector_embeddings", [])?;
        for record in records {
            insert_record(&transaction, record)?;
        }
        transaction.execute(
            "INSERT INTO cache_metadata(key, value) VALUES ('format_version', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert(&mut self, record: &Record) -> Result<(), CacheError> {
        let transaction = self.connection.transaction()?;
        let path = record.path.to_string_lossy();
        transaction.execute(
            "DELETE FROM records_fts WHERE model = ?1 AND (record_key = ?2 OR path = ?3)",
            params![record.model, record.key, path],
        )?;
        transaction.execute(
            "DELETE FROM records WHERE model = ?1 AND (record_key = ?2 OR path = ?3)",
            params![record.model, record.key, path],
        )?;
        insert_record(&transaction, record)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_path(&mut self, model: &str, path: &Path) -> Result<(), CacheError> {
        let transaction = self.connection.transaction()?;
        let path = path.to_string_lossy();
        transaction.execute(
            "DELETE FROM records_fts WHERE model = ?1 AND path = ?2",
            params![model, path],
        )?;
        transaction.execute(
            "DELETE FROM records WHERE model = ?1 AND path = ?2",
            params![model, path],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn query(&self, model: &str, query: &Query, page: usize) -> Result<Page, CacheError> {
        let page = page.max(1);
        let page_size = query.page_size.clamp(1, 1000);
        let mut clauses = vec!["model = ?".to_owned()];
        let mut parameters = vec![SqlValue::Text(model.to_owned())];

        for filter in &query.filters {
            let path = json_path(&filter.field);
            let expression = match filter.op {
                FilterOp::IsNull => {
                    parameters.push(SqlValue::Text(path));
                    "COALESCE(json_type(data, ?), 'null') = 'null'".to_owned()
                }
                FilterOp::IsNotNull => {
                    parameters.push(SqlValue::Text(path));
                    "COALESCE(json_type(data, ?), 'null') <> 'null'".to_owned()
                }
                FilterOp::In => {
                    let options = filter
                        .value
                        .as_ref()
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if options.is_empty() {
                        "0".to_owned()
                    } else {
                        parameters.push(SqlValue::Text(path));
                        let placeholders = options
                            .into_iter()
                            .map(|value| {
                                parameters.push(SqlValue::Text(value.to_string()));
                                "json_extract(?, '$')"
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("json_extract(data, ?) IN ({placeholders})")
                    }
                }
                FilterOp::Contains => {
                    let value = filter.value.clone().unwrap_or(Value::Null).to_string();
                    parameters.extend([
                        SqlValue::Text(path.clone()),
                        SqlValue::Text(path.clone()),
                        SqlValue::Text(value.clone()),
                        SqlValue::Text(path),
                        SqlValue::Text(value),
                    ]);
                    "(CASE json_type(data, ?)
                       WHEN 'array' THEN EXISTS (
                         SELECT 1 FROM json_each(json_extract(data, ?))
                         WHERE value IS json_extract(?, '$')
                       )
                       ELSE instr(CAST(json_extract(data, ?) AS TEXT), CAST(json_extract(?, '$') AS TEXT)) > 0
                     END)"
                        .to_owned()
                }
                operator => {
                    parameters.push(SqlValue::Text(path));
                    parameters.push(SqlValue::Text(
                        filter.value.clone().unwrap_or(Value::Null).to_string(),
                    ));
                    let operator = match operator {
                        FilterOp::Eq => "IS",
                        FilterOp::NotEq => "IS NOT",
                        FilterOp::Lt => "<",
                        FilterOp::Lte => "<=",
                        FilterOp::Gt => ">",
                        FilterOp::Gte => ">=",
                        FilterOp::Contains
                        | FilterOp::In
                        | FilterOp::IsNull
                        | FilterOp::IsNotNull => unreachable!(),
                    };
                    format!("json_extract(data, ?) {operator} json_extract(?, '$')")
                }
            };
            clauses.push(expression);
        }

        let where_clause = clauses.join(" AND ");
        let count_sql = format!("SELECT COUNT(*) FROM records WHERE {where_clause}");
        let total: usize =
            self.connection
                .query_row(&count_sql, params_from_iter(parameters.clone()), |row| {
                    row.get(0)
                })?;

        let mut order_parts = Vec::new();
        for order in &query.order {
            parameters.push(SqlValue::Text(json_path(&order.field)));
            let direction = match order.direction {
                Direction::Asc => "ASC",
                Direction::Desc => "DESC",
            };
            order_parts.push(format!("json_extract(data, ?) {direction}"));
        }
        order_parts.push("record_key ASC".to_owned());
        parameters.push(SqlValue::Integer(
            i64::try_from(page_size).unwrap_or(i64::MAX),
        ));
        parameters.push(SqlValue::Integer(
            i64::try_from((page - 1).saturating_mul(page_size)).unwrap_or(i64::MAX),
        ));
        let select_sql = format!(
            "SELECT model, record_key, path, data FROM records
             WHERE {where_clause}
             ORDER BY {}
             LIMIT ? OFFSET ?",
            order_parts.join(", ")
        );
        let mut statement = self.connection.prepare(&select_sql)?;
        let records = statement
            .query_map(params_from_iter(parameters), record_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            records,
            page,
            page_size,
            total,
            pages: total.div_ceil(page_size),
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, CacheError> {
        let mut statement = self.connection.prepare(
            "SELECT model, record_key, path, rank
             FROM records_fts
             WHERE records_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let hits = statement
            .query_map(
                params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    Ok(SearchHit {
                        model: row.get(0)?,
                        key: row.get(1)?,
                        path: row.get(2)?,
                        rank: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hits)
    }
}

fn insert_record(connection: &Connection, record: &Record) -> Result<(), CacheError> {
    let data = serde_json::to_string(&record.values)?;
    let content = record
        .values
        .values()
        .filter_map(|value| match value {
            Value::String(value) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let path = record.path.to_string_lossy();
    connection.execute(
        "INSERT INTO records(model, record_key, path, data) VALUES (?1, ?2, ?3, ?4)",
        params![record.model, record.key, path, data],
    )?;
    connection.execute(
        "INSERT INTO records_fts(model, record_key, path, content) VALUES (?1, ?2, ?3, ?4)",
        params![record.model, record.key, path, content],
    )?;
    Ok(())
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<Record> {
    let data: String = row.get(3)?;
    let values = serde_json::from_str::<BTreeMap<String, Value>>(&data).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            data.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(Record {
        model: row.get(0)?,
        key: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        values,
    })
}

fn json_path(field: &str) -> String {
    format!("$.\"{}\"", field.replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::tempdir;

    use omniapp_schema::{Direction, Filter, Order};

    use super::*;

    #[test]
    fn cache_is_rebuilt_and_searchable() {
        let directory = tempdir().unwrap();
        let mut cache = Cache::open(&directory.path().join("cache.sqlite3")).unwrap();
        cache
            .rebuild(&[Record {
                key: "dune".into(),
                model: "Book".into(),
                path: PathBuf::from("books/dune"),
                values: BTreeMap::from([
                    ("title".into(), json!("Dune")),
                    ("notes".into(), json!("Fear is the mind-killer")),
                ]),
            }])
            .unwrap();
        let hits = cache.search("mind", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "dune");

        cache.rebuild(&[]).unwrap();
        assert!(cache.search("mind", 10).unwrap().is_empty());
    }

    #[test]
    fn declarative_queries_execute_against_cached_json() {
        let directory = tempdir().unwrap();
        let mut cache = Cache::open(&directory.path().join("cache.sqlite3")).unwrap();
        cache
            .rebuild(&[
                record(
                    "late",
                    json!({"status":"scheduled", "date":"2026-08-02", "posted":null}),
                ),
                record(
                    "draft",
                    json!({"status":"draft", "date":"2026-07-01", "posted":null}),
                ),
                record(
                    "early",
                    json!({"status":"scheduled", "date":"2026-07-10", "posted":null}),
                ),
            ])
            .unwrap();
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
        let first = cache.query("Post", &query, 1).unwrap();
        let second = cache.query("Post", &query, 2).unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.records[0].key, "early");
        assert_eq!(second.records[0].key, "late");
    }

    fn record(key: &str, values: Value) -> Record {
        Record {
            key: key.into(),
            model: "Post".into(),
            path: PathBuf::from(format!("posts/{key}.md")),
            values: serde_json::from_value(values).unwrap(),
        }
    }
}
