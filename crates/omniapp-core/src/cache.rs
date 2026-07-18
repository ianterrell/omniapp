use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use omniapp_schema::{Direction, FilterOp, Model, Query};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row, params, params_from_iter};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::{Group, GroupedPage, Page, Record};

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("could not serialize record for cache: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("invalid relational query field {0:?}")]
    InvalidFieldPath(String),
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
        let cache_version: u32 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if cache_version != 3 {
            connection.execute_batch(
                "DROP TABLE IF EXISTS records;
                 DROP TABLE IF EXISTS records_fts;
                 DROP TABLE IF EXISTS vector_embeddings;
                 DROP TABLE IF EXISTS cache_metadata;
                 PRAGMA user_version = 3;",
            )?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS cache_metadata (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS records (
               model TEXT NOT NULL,
               record_key TEXT NOT NULL,
               path TEXT NOT NULL,
               revision TEXT NOT NULL,
               fingerprint TEXT NOT NULL,
               data TEXT NOT NULL,
               PRIMARY KEY(model, record_key)
             );
             CREATE INDEX IF NOT EXISTS records_by_path ON records(model, path);
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

    pub fn rebuild(&mut self, records: &[(Record, String)]) -> Result<(), CacheError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM records", [])?;
        transaction.execute("DELETE FROM records_fts", [])?;
        transaction.execute("DELETE FROM vector_embeddings", [])?;
        for (record, fingerprint) in records {
            insert_record(&transaction, record, fingerprint)?;
        }
        transaction.execute(
            "INSERT INTO cache_metadata(key, value) VALUES ('format_version', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert(&mut self, record: &Record, fingerprint: &str) -> Result<(), CacheError> {
        let transaction = self.connection.transaction()?;
        upsert_in_transaction(&transaction, record, fingerprint)?;
        transaction.commit()?;
        Ok(())
    }

    /// Apply an incremental batch of upserts and removals in one transaction.
    /// Removals are `(model, path)` pairs.
    ///
    /// Displaced rows are deleted in one pass per table through an indexed
    /// temp table: per-row `DELETE`s against the FTS table cannot use an
    /// index, so they would scan it once per record — quadratic on a cold
    /// sync.
    pub fn apply(
        &mut self,
        upserts: &[(Record, String)],
        removals: &[(String, String)],
    ) -> Result<(), CacheError> {
        if upserts.is_empty() && removals.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        transaction.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS pending (model TEXT, record_key TEXT, path TEXT);
             CREATE INDEX IF NOT EXISTS temp.pending_by_key ON pending(model, record_key);
             CREATE INDEX IF NOT EXISTS temp.pending_by_path ON pending(model, path);
             DELETE FROM pending;",
        )?;
        {
            let mut insert_pending = transaction.prepare_cached(
                "INSERT INTO pending(model, record_key, path) VALUES (?1, ?2, ?3)",
            )?;
            for (model, path) in removals {
                insert_pending.execute(params![model, Option::<String>::None, path])?;
            }
            for (record, _) in upserts {
                insert_pending.execute(params![
                    record.model,
                    record.key,
                    record.path.to_string_lossy()
                ])?;
            }
        }
        for table in ["records_fts", "records"] {
            transaction.execute(
                &format!(
                    "DELETE FROM {table} WHERE
                       EXISTS (SELECT 1 FROM pending
                               WHERE pending.model = {table}.model
                                 AND pending.record_key = {table}.record_key)
                       OR EXISTS (SELECT 1 FROM pending
                                  WHERE pending.model = {table}.model
                                    AND pending.path = {table}.path)"
                ),
                [],
            )?;
        }
        for (record, fingerprint) in upserts {
            insert_record(&transaction, record, fingerprint)?;
        }
        transaction.execute("DELETE FROM pending", [])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn metadata(&self, key: &str) -> Result<Option<String>, CacheError> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM cache_metadata WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(value)
    }

    pub fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), CacheError> {
        self.connection.execute(
            "INSERT INTO cache_metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every cached record's stat fingerprint, keyed by `(model, path)`.
    pub fn fingerprints(&self) -> Result<HashMap<(String, String), String>, CacheError> {
        let mut statement = self
            .connection
            .prepare("SELECT model, path, fingerprint FROM records")?;
        let entries = statement
            .query_map([], |row| {
                Ok(((row.get(0)?, row.get(1)?), row.get::<_, String>(2)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(entries)
    }

    /// Every cached record, ordered by model then path — the same order a
    /// full filesystem scan produces.
    pub fn all_records(&self) -> Result<Vec<Record>, CacheError> {
        let mut statement = self.connection.prepare(
            "SELECT model, record_key, path, revision, data FROM records ORDER BY model, path",
        )?;
        let records = statement
            .query_map([], record_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
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

    /// Run a declarative query, optionally narrowed by a case-insensitive
    /// substring search across the record key and every string value
    /// (including strings nested in arrays and objects).
    pub fn query(
        &self,
        model: &str,
        query: &Query,
        page: usize,
        search: Option<&str>,
    ) -> Result<Page, CacheError> {
        self.query_internal(model, None, query, page, search)
    }

    /// Run a declarative query whose filter and order fields may traverse
    /// single-valued references with dotted paths.
    pub fn query_with_relations(
        &self,
        model: &str,
        models: &BTreeMap<String, Model>,
        query: &Query,
        page: usize,
        search: Option<&str>,
    ) -> Result<Page, CacheError> {
        self.query_internal(model, Some(models), query, page, search)
    }

    fn query_internal(
        &self,
        model: &str,
        models: Option<&BTreeMap<String, Model>>,
        query: &Query,
        page: usize,
        search: Option<&str>,
    ) -> Result<Page, CacheError> {
        let page = page.max(1);
        let page_size = query.page_size.clamp(1, 1000);
        let mut parameters = Vec::new();
        let clauses = build_where_clauses(model, models, query, search, &mut parameters)?;

        let where_clause = clauses.join(" AND ");
        let count_sql = format!("SELECT COUNT(*) FROM records WHERE {where_clause}");
        let total: usize =
            self.connection
                .query_row(&count_sql, params_from_iter(parameters.clone()), |row| {
                    row.get(0)
                })?;

        let order_parts = build_order_parts(model, models, query, &mut parameters)?;
        parameters.push(SqlValue::Integer(
            i64::try_from(page_size).unwrap_or(i64::MAX),
        ));
        parameters.push(SqlValue::Integer(
            i64::try_from((page - 1).saturating_mul(page_size)).unwrap_or(i64::MAX),
        ));
        let select_sql = format!(
            "SELECT model, record_key, path, revision, data FROM records
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

    /// Run a declarative query and bucket the matches by `group_by` (which may
    /// be a dotted reference path), preserving the order the records appear in.
    /// Each group keeps at most `group_limit` records; the group's full size is
    /// reported separately so callers can show an overflow note.
    pub fn query_grouped(
        &self,
        model: &str,
        models: &BTreeMap<String, Model>,
        query: &Query,
        group_by: &str,
        group_limit: Option<usize>,
        search: Option<&str>,
    ) -> Result<GroupedPage, CacheError> {
        let models = Some(models);
        // Parameters are bound positionally, so they must be pushed in the
        // order the placeholders appear in the SQL text: SELECT (group value),
        // then WHERE, then ORDER BY.
        let mut parameters = Vec::new();
        let group_expr = field_expression(model, models, group_by, &mut parameters)?;
        let clauses = build_where_clauses(model, models, query, search, &mut parameters)?;
        let where_clause = clauses.join(" AND ");
        let order_parts = build_order_parts(model, models, query, &mut parameters)?;
        // Grouping needs every match so per-group limits are correct; cap at the
        // query engine's ceiling rather than paginating.
        let select_sql = format!(
            "SELECT model, record_key, path, revision, data, ({group_expr}) AS group_value
             FROM records
             WHERE {where_clause}
             ORDER BY {}
             LIMIT 1000",
            order_parts.join(", ")
        );
        let mut statement = self.connection.prepare(&select_sql)?;
        let rows = statement
            .query_map(params_from_iter(parameters), |row| {
                Ok((record_from_row(row)?, group_value_from_row(row)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let total = rows.len();
        let mut order: Vec<Option<Value>> = Vec::new();
        let mut buckets: HashMap<String, (Vec<Record>, usize)> = HashMap::new();
        for (record, value) in rows {
            let bucket_key = match &value {
                Some(value) => value.to_string(),
                None => "\u{0}none".to_owned(),
            };
            let entry = buckets.entry(bucket_key).or_insert_with(|| {
                order.push(value.clone());
                (Vec::new(), 0)
            });
            entry.1 += 1;
            if group_limit.is_none_or(|limit| entry.0.len() < limit) {
                entry.0.push(record);
            }
        }
        let groups = order
            .into_iter()
            .map(|value| {
                let bucket_key = match &value {
                    Some(value) => value.to_string(),
                    None => "\u{0}none".to_owned(),
                };
                let (records, total) = buckets.remove(&bucket_key).unwrap_or_default();
                Group {
                    value,
                    records,
                    total,
                }
            })
            .collect();
        Ok(GroupedPage {
            groups,
            total,
            grouped: true,
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

fn upsert_in_transaction(
    connection: &Connection,
    record: &Record,
    fingerprint: &str,
) -> Result<(), CacheError> {
    let path = record.path.to_string_lossy();
    connection.execute(
        "DELETE FROM records_fts WHERE model = ?1 AND (record_key = ?2 OR path = ?3)",
        params![record.model, record.key, path],
    )?;
    connection.execute(
        "DELETE FROM records WHERE model = ?1 AND (record_key = ?2 OR path = ?3)",
        params![record.model, record.key, path],
    )?;
    insert_record(connection, record, fingerprint)
}

fn insert_record(
    connection: &Connection,
    record: &Record,
    fingerprint: &str,
) -> Result<(), CacheError> {
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
    connection
        .prepare_cached(
            "INSERT INTO records(model, record_key, path, revision, fingerprint, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            record.model,
            record.key,
            path,
            record.revision,
            fingerprint,
            data
        ])?;
    connection
        .prepare_cached(
            "INSERT INTO records_fts(model, record_key, path, content) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![record.model, record.key, path, content])?;
    Ok(())
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<Record> {
    let data: String = row.get(4)?;
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
        revision: row.get(3)?,
        values,
    })
}

fn json_path(field: &str) -> String {
    format!("$.\"{}\"", field.replace('"', "\\\""))
}

/// Build the WHERE clause fragments (model, optional search, filters) shared by
/// the paginated and grouped query paths, pushing bound parameters in order.
fn build_where_clauses(
    model: &str,
    models: Option<&BTreeMap<String, Model>>,
    query: &Query,
    search: Option<&str>,
    parameters: &mut Vec<SqlValue>,
) -> Result<Vec<String>, CacheError> {
    let mut clauses = vec!["model = ?".to_owned()];
    parameters.push(SqlValue::Text(model.to_owned()));

    if let Some(needle) = search.map(str::trim).filter(|needle| !needle.is_empty()) {
        parameters.push(SqlValue::Text(needle.to_owned()));
        parameters.push(SqlValue::Text(needle.to_owned()));
        clauses.push(
            "(EXISTS (SELECT 1 FROM json_tree(records.data)
                      WHERE json_tree.type = 'text'
                        AND instr(lower(json_tree.value), lower(?)) > 0)
              OR instr(lower(record_key), lower(?)) > 0)"
                .to_owned(),
        );
    }

    for filter in &query.filters {
        let expression = match filter.op {
            FilterOp::IsNull => {
                let field = field_expression(model, models, &filter.field, parameters)?;
                format!("({field}) IS NULL")
            }
            FilterOp::IsNotNull => {
                let field = field_expression(model, models, &filter.field, parameters)?;
                format!("({field}) IS NOT NULL")
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
                    let field = field_expression(model, models, &filter.field, parameters)?;
                    let placeholders = options
                        .into_iter()
                        .map(|value| {
                            parameters.push(SqlValue::Text(value.to_string()));
                            "json_extract(?, '$')"
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{field} IN ({placeholders})")
                }
            }
            FilterOp::Contains => {
                let value = filter.value.clone().unwrap_or(Value::Null).to_string();
                let type_field = field_expression(model, models, &filter.field, parameters)?;
                let array_field = field_expression(model, models, &filter.field, parameters)?;
                parameters.push(SqlValue::Text(value.clone()));
                let text_field = field_expression(model, models, &filter.field, parameters)?;
                parameters.push(SqlValue::Text(value));
                format!(
                    "(CASE json_type({type_field})
                   WHEN 'array' THEN EXISTS (
                     SELECT 1 FROM json_each({array_field})
                     WHERE value IS json_extract(?, '$')
                   )
                   ELSE instr(CAST({text_field} AS TEXT), CAST(json_extract(?, '$') AS TEXT)) > 0
                 END)"
                )
            }
            operator => {
                let field = field_expression(model, models, &filter.field, parameters)?;
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
                    FilterOp::Contains | FilterOp::In | FilterOp::IsNull | FilterOp::IsNotNull => {
                        unreachable!()
                    }
                };
                format!("{field} {operator} json_extract(?, '$')")
            }
        };
        clauses.push(expression);
    }
    Ok(clauses)
}

/// Build the ORDER BY fragments (query order plus a stable key tiebreak),
/// pushing bound parameters in order.
fn build_order_parts(
    model: &str,
    models: Option<&BTreeMap<String, Model>>,
    query: &Query,
    parameters: &mut Vec<SqlValue>,
) -> Result<Vec<String>, CacheError> {
    let mut order_parts = Vec::new();
    for order in &query.order {
        let field = field_expression(model, models, &order.field, parameters)?;
        let direction = match order.direction {
            Direction::Asc => "ASC",
            Direction::Desc => "DESC",
        };
        order_parts.push(format!("{field} {direction}"));
    }
    order_parts.push("record_key ASC".to_owned());
    Ok(order_parts)
}

/// Read the trailing `group_value` column of a grouped query row into JSON.
fn group_value_from_row(row: &Row<'_>) -> rusqlite::Result<Option<Value>> {
    let value: SqlValue = row.get(5)?;
    Ok(match value {
        SqlValue::Integer(number) => Some(Value::from(number)),
        SqlValue::Real(number) => Some(Value::from(number)),
        SqlValue::Text(text) => Some(Value::from(text)),
        SqlValue::Null | SqlValue::Blob(_) => None,
    })
}

fn field_expression(
    root_model: &str,
    models: Option<&BTreeMap<String, Model>>,
    field_path: &str,
    parameters: &mut Vec<SqlValue>,
) -> Result<String, CacheError> {
    let segments = field_path.split('.').collect::<Vec<_>>();
    if segments.len() == 1 {
        parameters.push(SqlValue::Text(json_path(field_path)));
        return Ok("json_extract(records.data, ?)".to_owned());
    }
    let models = models.ok_or_else(|| CacheError::InvalidFieldPath(field_path.to_owned()))?;
    let mut current_model = models
        .get(root_model)
        .ok_or_else(|| CacheError::InvalidFieldPath(field_path.to_owned()))?;
    let mut steps = Vec::new();
    for segment in &segments[..segments.len() - 1] {
        let field = current_model
            .fields
            .get(*segment)
            .ok_or_else(|| CacheError::InvalidFieldPath(field_path.to_owned()))?;
        let reference = field
            .reference
            .as_ref()
            .filter(|reference| !reference.many)
            .ok_or_else(|| CacheError::InvalidFieldPath(field_path.to_owned()))?;
        steps.push((
            (*segment).to_owned(),
            reference.model.clone(),
            reference.field.clone(),
        ));
        current_model = models
            .get(&reference.model)
            .ok_or_else(|| CacheError::InvalidFieldPath(field_path.to_owned()))?;
    }
    let final_field = segments[segments.len() - 1];
    if !current_model.fields.contains_key(final_field) {
        return Err(CacheError::InvalidFieldPath(field_path.to_owned()));
    }

    parameters.push(SqlValue::Text(json_path(final_field)));
    let final_alias = format!("related_{}", steps.len() - 1);
    let mut sql = format!("(SELECT json_extract({final_alias}.data, ?) FROM records AS related_0");
    for (index, (source_field, target_model, target_field)) in steps.iter().enumerate().skip(1) {
        let source_alias = format!("related_{}", index - 1);
        let target_alias = format!("related_{index}");
        parameters.extend([
            SqlValue::Text(target_model.clone()),
            SqlValue::Text(json_path(target_field)),
            SqlValue::Text(json_path(source_field)),
        ]);
        let _ = write!(
            sql,
            " JOIN records AS {target_alias} ON {target_alias}.model = ? \
             AND json_extract({target_alias}.data, ?) IS json_extract({source_alias}.data, ?)"
        );
    }
    let (source_field, target_model, target_field) = &steps[0];
    parameters.extend([
        SqlValue::Text(target_model.clone()),
        SqlValue::Text(json_path(target_field)),
        SqlValue::Text(json_path(source_field)),
    ]);
    sql.push_str(
        " WHERE related_0.model = ? \
         AND json_extract(related_0.data, ?) IS json_extract(records.data, ?) LIMIT 1)",
    );
    Ok(sql)
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
            .rebuild(&[(
                Record {
                    key: "dune".into(),
                    model: "Book".into(),
                    path: PathBuf::from("books/dune"),
                    revision: "test".into(),
                    values: BTreeMap::from([
                        ("title".into(), json!("Dune")),
                        ("notes".into(), json!("Fear is the mind-killer")),
                    ]),
                },
                "books/dune=1:1".to_owned(),
            )])
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
        let first = cache.query("Post", &query, 1, None).unwrap();
        let second = cache.query("Post", &query, 2, None).unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.records[0].key, "early");
        assert_eq!(second.records[0].key, "late");
    }

    #[test]
    fn cached_queries_filter_through_references() {
        let directory = tempdir().unwrap();
        let mut cache = Cache::open(&directory.path().join("cache.sqlite3")).unwrap();
        cache
            .rebuild(&[
                typed_record(
                    "active-todo",
                    "Todo",
                    json!({"done": false, "book": "active"}),
                ),
                typed_record(
                    "archived-todo",
                    "Todo",
                    json!({"done": false, "book": "archived"}),
                ),
                typed_record("general-todo", "Todo", json!({"done": false})),
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
            ])
            .unwrap();
        let todo_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Todo
storage: { kind: file, path: "todos/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
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
        let query = Query {
            filters: vec![Filter {
                field: "book.publication_state".into(),
                op: FilterOp::NotEq,
                value: Some(json!("archived")),
            }],
            order: vec![],
            page_size: 50,
        };
        let page = cache
            .query_with_relations("Todo", &models, &query, 1, None)
            .unwrap();
        assert_eq!(
            page.records
                .iter()
                .map(|record| record.key.as_str())
                .collect::<Vec<_>>(),
            vec!["active-todo", "general-todo"]
        );
    }

    #[test]
    fn grouped_query_buckets_by_reference_path_with_per_group_limit() {
        let directory = tempdir().unwrap();
        let mut cache = Cache::open(&directory.path().join("cache.sqlite3")).unwrap();
        cache
            .rebuild(&[
                typed_record(
                    "t1",
                    "Todo",
                    json!({"done": false, "priority": 1, "project": "quick"}),
                ),
                typed_record(
                    "t2",
                    "Todo",
                    json!({"done": false, "priority": 2, "project": "quick"}),
                ),
                typed_record(
                    "t3",
                    "Todo",
                    json!({"done": false, "priority": 3, "project": "quick"}),
                ),
                typed_record(
                    "t4",
                    "Todo",
                    json!({"done": false, "priority": 1, "project": "snack"}),
                ),
                typed_record(
                    "gone",
                    "Todo",
                    json!({"done": true, "priority": 1, "project": "quick"}),
                ),
                typed_record("quick", "Project", json!({"slug": "quick", "brand": "q"})),
                typed_record("snack", "Project", json!({"slug": "snack", "brand": "s"})),
                typed_record("q", "Brand", json!({"slug": "q", "name": "Quickies"})),
                typed_record("s", "Brand", json!({"slug": "s", "name": "Snacks"})),
            ])
            .unwrap();
        let todo_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Todo
storage: { kind: file, path: "todos/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
  done: { type: boolean, source: { kind: frontmatter, key: done } }
  priority: { type: integer, source: { kind: frontmatter, key: priority } }
  project:
    type: reference
    source: { kind: frontmatter, key: project }
    reference: { model: Project, field: slug }
"#,
        )
        .unwrap();
        let project_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Project
storage: { kind: file, path: "projects/{slug}.md" }
fields:
  slug: { type: string, source: { kind: path, variable: slug } }
  brand:
    type: reference
    source: { kind: frontmatter, key: brand }
    reference: { model: Brand, field: slug }
"#,
        )
        .unwrap();
        let brand_model: Model = serde_yaml::from_str(
            r#"
version: 1
name: Brand
storage: { kind: file, path: "brands/{slug}.md" }
fields:
  slug: { type: string, source: { kind: path, variable: slug } }
  name: { type: string, source: { kind: frontmatter, key: name } }
"#,
        )
        .unwrap();
        let models = BTreeMap::from([
            ("Todo".to_owned(), todo_model),
            ("Project".to_owned(), project_model),
            ("Brand".to_owned(), brand_model),
        ]);
        let query = Query {
            filters: vec![Filter {
                field: "done".into(),
                op: FilterOp::NotEq,
                value: Some(json!(true)),
            }],
            order: vec![
                Order {
                    field: "project.brand.name".into(),
                    direction: Direction::Asc,
                },
                Order {
                    field: "priority".into(),
                    direction: Direction::Asc,
                },
            ],
            page_size: 100,
        };
        let grouped = cache
            .query_grouped("Todo", &models, &query, "project.brand.name", Some(2), None)
            .unwrap();

        // The done todo is filtered out; groups are labeled by the resolved
        // brand name and ordered by it.
        assert_eq!(grouped.total, 4);
        let labels: Vec<&str> = grouped
            .groups
            .iter()
            .map(|group| group.value.as_ref().and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(labels, vec!["Quickies", "Snacks"]);

        // Quickies has three active todos but only the top two are kept.
        let quickies = &grouped.groups[0];
        assert_eq!(quickies.total, 3);
        assert_eq!(
            quickies
                .records
                .iter()
                .map(|record| record.key.as_str())
                .collect::<Vec<_>>(),
            vec!["t1", "t2"]
        );
        let snacks = &grouped.groups[1];
        assert_eq!(snacks.total, 1);
        assert_eq!(snacks.records.len(), 1);
    }

    #[test]
    fn substring_search_spans_all_pages_and_respects_filters() {
        let directory = tempdir().unwrap();
        let mut cache = Cache::open(&directory.path().join("cache.sqlite3")).unwrap();
        cache
            .rebuild(&[
                record(
                    "sqlite-cache",
                    json!({"status":"published", "title":"SQLite as a cache", "tags":["sqlite","architecture"]}),
                ),
                record(
                    "filesystem",
                    json!({"status":"published", "title":"Why the filesystem wins", "tags":["local-first"]}),
                ),
                record(
                    "sqlite-draft",
                    json!({"status":"draft", "title":"More SQLite thoughts", "tags":[]}),
                ),
            ])
            .unwrap();
        let query = Query {
            filters: vec![Filter {
                field: "status".into(),
                op: FilterOp::Eq,
                value: Some(json!("published")),
            }],
            order: vec![],
            page_size: 1,
        };
        // Case-insensitive match on a title, combined with the status filter.
        let hits = cache.query("Post", &query, 1, Some("sqlite")).unwrap();
        assert_eq!(hits.total, 1);
        assert_eq!(hits.records[0].key, "sqlite-cache");
        // Matches strings nested inside arrays.
        let nested = cache.query("Post", &query, 1, Some("local-first")).unwrap();
        assert_eq!(nested.total, 1);
        assert_eq!(nested.records[0].key, "filesystem");
        // Matches the record key itself; blank search is a no-op.
        assert_eq!(
            cache
                .query("Post", &query, 1, Some("filesys"))
                .unwrap()
                .total,
            1
        );
        assert_eq!(cache.query("Post", &query, 1, Some("  ")).unwrap().total, 2);
    }

    fn record(key: &str, values: Value) -> (Record, String) {
        typed_record(key, "Post", values)
    }

    fn typed_record(key: &str, model: &str, values: Value) -> (Record, String) {
        (
            Record {
                key: key.into(),
                model: model.into(),
                path: PathBuf::from(format!("{model}/{key}.md")),
                revision: "test".into(),
                values: serde_json::from_value(values).unwrap(),
            },
            format!("posts/{key}.md=1:1"),
        )
    }
}
