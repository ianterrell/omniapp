use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub model: String,
    pub path: PathBuf,
    pub revision: String,
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordInput {
    #[serde(default)]
    pub revision: Option<String>,
    pub values: BTreeMap<String, Value>,
}
