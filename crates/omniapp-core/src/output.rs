use std::path::PathBuf;

use omniapp_schema::OutputKind;
use serde::Serialize;

use crate::Record;

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedOutput {
    pub name: String,
    pub path: PathBuf,
    pub kind: OutputKind,
    pub exists: bool,
    pub is_file: bool,
    pub is_directory: bool,
    /// For directory outputs that exist: every file inside, recursive, sorted,
    /// relative to `path`. Always empty for file outputs.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputSet {
    pub record: Record,
    pub outputs: Vec<GeneratedOutput>,
}
