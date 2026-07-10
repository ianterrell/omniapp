use std::path::PathBuf;

use serde::Serialize;

use crate::Record;

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedOutput {
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
    pub is_file: bool,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputSet {
    pub record: Record,
    pub outputs: Vec<GeneratedOutput>,
}
