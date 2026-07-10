use std::path::Path;

use rusqlite::{Connection, params};
use serde::Serialize;
use thiserror::Error;

use crate::Record;

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
            let data = serde_json::to_string(&record.values)?;
            let content = record
                .values
                .values()
                .filter_map(|value| match value {
                    serde_json::Value::String(value) => Some(value.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let path = record.path.to_string_lossy();
            transaction.execute(
                "INSERT INTO records(model, record_key, path, data) VALUES (?1, ?2, ?3, ?4)",
                params![record.model, record.key, path, data],
            )?;
            transaction.execute(
                "INSERT INTO records_fts(model, record_key, path, content) VALUES (?1, ?2, ?3, ?4)",
                params![record.model, record.key, path, content],
            )?;
        }
        transaction.execute(
            "INSERT INTO cache_metadata(key, value) VALUES ('format_version', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        transaction.commit()?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::tempdir;

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
}
