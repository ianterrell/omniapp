//! Declarative, stable-on-disk project format for OmniApp.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not parse {path}: {source}")]
    Parse {
        path: String,
        source: serde_yaml::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub storage: Storage,
    pub fields: BTreeMap<String, Field>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Storage {
    /// Each record is a directory containing one or more configured files.
    Directory { path: String },
    /// Each record is one Markdown file whose fields live in its body/frontmatter.
    File { path: String },
}

impl Storage {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path } => path,
        }
    }

    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    pub source: FieldSource,
    #[serde(default)]
    pub validation: Validation,
    #[serde(default)]
    pub reference: Option<Reference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Text,
    Integer,
    Number,
    Boolean,
    Date,
    DateTime,
    Enum,
    Reference,
    Asset,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldSource {
    /// The value is captured from a placeholder in the model storage path.
    Path { variable: String },
    /// The value is a key in a YAML mapping, shared by any number of fields.
    Yaml { file: String, key: String },
    /// The Markdown body after optional YAML frontmatter.
    Markdown {
        /// Required for directory records; omitted for single-file records.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    /// A key in the YAML frontmatter of a Markdown document.
    Frontmatter {
        /// Required for directory records; omitted for single-file records.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        key: String,
    },
    /// The field value is the project-relative path to a file in the record directory.
    Asset { file: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Validation {
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub choices: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub model: String,
    #[serde(default = "default_reference_field")]
    pub field: String,
    #[serde(default)]
    pub many: bool,
}

fn default_reference_field() -> String {
    "id".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub version: u32,
    pub name: String,
    pub model: String,
    #[serde(rename = "type")]
    pub view_type: ViewType,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub query: Query,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub actions: Vec<ViewAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewType {
    Form,
    Table,
    Tree,
    Board,
    Calendar,
    Gallery,
    Timeline,
    Custom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub order: Vec<Order>,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

fn default_page_size() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    In,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub field: String,
    #[serde(default)]
    pub direction: Direction,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewAction {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub script: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub location: String,
    pub message: String,
}

impl Problem {
    #[must_use]
    pub fn new(location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            message: message.into(),
        }
    }
}

pub fn read_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SchemaError> {
    let contents = std::fs::read_to_string(path).map_err(|source| SchemaError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_yaml::from_str(&contents).map_err(|source| SchemaError::Parse {
        path: path.display().to_string(),
        source,
    })
}

#[must_use]
pub fn validate_config(config: &ProjectConfig) -> Vec<Problem> {
    let mut problems = Vec::new();
    if config.version != FORMAT_VERSION {
        problems.push(Problem::new(
            "config.version",
            format!(
                "unsupported format version {}; expected {FORMAT_VERSION}",
                config.version
            ),
        ));
    }
    if config.name.trim().is_empty() {
        problems.push(Problem::new("config.name", "must not be empty"));
    }
    problems
}

#[must_use]
pub fn validate_model(model: &Model) -> Vec<Problem> {
    let mut problems = Vec::new();
    let location = format!("model {}", model.name);
    if model.version != FORMAT_VERSION {
        problems.push(Problem::new(
            format!("{location}.version"),
            format!(
                "unsupported format version {}; expected {FORMAT_VERSION}",
                model.version
            ),
        ));
    }
    if model.name.trim().is_empty() {
        problems.push(Problem::new(&location, "name must not be empty"));
    }
    let storage_path = model.storage.path();
    if storage_path.starts_with('/') || storage_path.split('/').any(|part| part == "..") {
        problems.push(Problem::new(
            format!("{location}.storage.path"),
            "must be a safe project-relative path",
        ));
    }
    if storage_path.split('/').next() == Some(".omniapp") {
        problems.push(Problem::new(
            format!("{location}.storage.path"),
            "record storage must live outside .omniapp",
        ));
    }
    if !valid_path_template(storage_path) {
        problems.push(Problem::new(
            format!("{location}.storage.path"),
            "contains malformed or ambiguous placeholders",
        ));
    }
    let placeholders = path_placeholders(storage_path);
    for placeholder in &placeholders {
        match model.fields.get(placeholder) {
            Some(Field {
                source: FieldSource::Path { variable },
                ..
            }) if variable == placeholder => {}
            _ => problems.push(Problem::new(
                format!("{location}.storage.path"),
                format!("placeholder {{{placeholder}}} needs a matching path-sourced field"),
            )),
        }
    }
    for (name, field) in &model.fields {
        let field_location = format!("{location}.fields.{name}");
        match &field.source {
            FieldSource::Path { variable } if !placeholders.contains(variable) => {
                problems.push(Problem::new(
                    &field_location,
                    format!("path variable {variable:?} is not in storage.path"),
                ));
            }
            FieldSource::Yaml { file, .. } | FieldSource::Asset { file }
                if !is_safe_relative(file) =>
            {
                problems.push(Problem::new(
                    &field_location,
                    "source file must be a safe record-relative path",
                ));
            }
            FieldSource::Markdown { file } | FieldSource::Frontmatter { file, .. } => {
                match (&model.storage, file) {
                    (Storage::Directory { .. }, Some(file)) if !is_safe_relative(file) => {
                        problems.push(Problem::new(
                            &field_location,
                            "source file must be a safe record-relative path",
                        ));
                    }
                    (Storage::Directory { .. }, None) => problems.push(Problem::new(
                        &field_location,
                        "directory records require source.file",
                    )),
                    (Storage::File { .. }, Some(_)) => problems.push(Problem::new(
                        &field_location,
                        "single-file records must omit source.file",
                    )),
                    _ => {}
                }
            }
            _ => {}
        }
        if model.storage.is_file()
            && matches!(
                &field.source,
                FieldSource::Yaml { .. } | FieldSource::Asset { .. }
            )
        {
            problems.push(Problem::new(
                &field_location,
                "single-file records support path, markdown, and frontmatter sources only",
            ));
        }
        if field.field_type == FieldType::Reference && field.reference.is_none() {
            problems.push(Problem::new(
                &field_location,
                "reference fields require reference configuration",
            ));
        }
        if field.field_type != FieldType::Reference && field.reference.is_some() {
            problems.push(Problem::new(
                &field_location,
                "reference configuration is only valid for reference fields",
            ));
        }
        if field.field_type == FieldType::Enum && field.validation.choices.is_empty() {
            problems.push(Problem::new(
                &field_location,
                "enum fields require validation.choices",
            ));
        }
        if let Some(pattern) = &field.validation.pattern
            && let Err(error) = regex::Regex::new(pattern)
        {
            problems.push(Problem::new(
                &field_location,
                format!("invalid validation pattern: {error}"),
            ));
        }
    }
    if model.storage.is_file()
        && !model.fields.values().any(|field| {
            matches!(
                &field.source,
                FieldSource::Markdown { .. } | FieldSource::Frontmatter { .. }
            )
        })
    {
        problems.push(Problem::new(
            format!("{location}.fields"),
            "single-file records require at least one markdown or frontmatter field",
        ));
    }

    let mut markdown_bodies = BTreeSet::new();
    let mut document_keys = BTreeSet::new();
    for (name, field) in &model.fields {
        match &field.source {
            FieldSource::Markdown { file } => {
                let document = file.as_deref().unwrap_or("<record-file>");
                if !markdown_bodies.insert(document.to_owned()) {
                    problems.push(Problem::new(
                        format!("{location}.fields.{name}"),
                        format!("document {document:?} already has a markdown body field"),
                    ));
                }
            }
            FieldSource::Frontmatter { file, key } => {
                let document = file.as_deref().unwrap_or("<record-file>");
                if !document_keys.insert((document.to_owned(), key.clone())) {
                    problems.push(Problem::new(
                        format!("{location}.fields.{name}"),
                        format!("frontmatter key {key:?} is configured more than once"),
                    ));
                }
            }
            FieldSource::Path { .. } | FieldSource::Yaml { .. } | FieldSource::Asset { .. } => {}
        }
    }
    for (name, path) in &model.outputs {
        if path.starts_with('/') || path.split('/').any(|part| part == "..") {
            problems.push(Problem::new(
                format!("{location}.outputs.{name}"),
                "must be a safe project-relative path template",
            ));
        }
    }
    problems
}

#[must_use]
pub fn validate_view(view: &View, models: &BTreeMap<String, Model>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let location = format!("view {}", view.name);
    if view.version != FORMAT_VERSION {
        problems.push(Problem::new(
            format!("{location}.version"),
            format!(
                "unsupported format version {}; expected {FORMAT_VERSION}",
                view.version
            ),
        ));
    }
    let Some(model) = models.get(&view.model) else {
        problems.push(Problem::new(
            format!("{location}.model"),
            format!("unknown model {:?}", view.model),
        ));
        return problems;
    };
    let check_field = |field: &str, suffix: &str, problems: &mut Vec<Problem>| {
        if !model.fields.contains_key(field) {
            problems.push(Problem::new(
                format!("{location}.{suffix}"),
                format!("unknown field {field:?} on model {}", model.name),
            ));
        }
    };
    for field in &view.fields {
        check_field(field, "fields", &mut problems);
    }
    for filter in &view.query.filters {
        check_field(&filter.field, "query.filters", &mut problems);
        let needs_value = !matches!(filter.op, FilterOp::IsNull | FilterOp::IsNotNull);
        if needs_value != filter.value.is_some() {
            problems.push(Problem::new(
                format!("{location}.query.filters.{}", filter.field),
                if needs_value {
                    "operator requires a value"
                } else {
                    "operator does not accept a value"
                },
            ));
        }
    }
    for order in &view.query.order {
        check_field(&order.field, "query.order", &mut problems);
    }
    if let Some(group_by) = &view.group_by {
        check_field(group_by, "group_by", &mut problems);
    }
    if view.query.page_size == 0 || view.query.page_size > 1000 {
        problems.push(Problem::new(
            format!("{location}.query.page_size"),
            "must be between 1 and 1000",
        ));
    }
    problems
}

#[must_use]
pub fn path_placeholders(template: &str) -> BTreeSet<String> {
    let mut placeholders = BTreeSet::new();
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        let after_start = &remaining[start + 1..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let variable = &after_start[..end];
        if !variable.is_empty() {
            placeholders.insert(variable.to_owned());
        }
        remaining = &after_start[end + 1..];
    }
    placeholders
}

#[must_use]
pub fn valid_path_template(template: &str) -> bool {
    if !is_safe_relative(template) {
        return false;
    }
    for segment in template.split('/') {
        let mut index = 0;
        let mut previous_was_placeholder = false;
        let mut placeholders = 0;
        while index < segment.len() {
            let remaining = &segment[index..];
            if remaining.starts_with('}') {
                return false;
            }
            if let Some(after_start) = remaining.strip_prefix('{') {
                if previous_was_placeholder {
                    return false;
                }
                placeholders += 1;
                if placeholders > 1 {
                    return false;
                }
                let Some(end) = after_start.find('}') else {
                    return false;
                };
                let variable = &after_start[..end];
                if variable.is_empty()
                    || !variable
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
                {
                    return false;
                }
                index += end + 2;
                previous_was_placeholder = true;
            } else {
                let length = remaining.chars().next().map_or(1, char::len_utf8);
                index += length;
                previous_was_placeholder = false;
            }
        }
    }
    true
}

#[must_use]
pub fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_path_placeholders() {
        assert_eq!(
            path_placeholders("books/{book}/scenes/{slug}"),
            BTreeSet::from(["book".to_owned(), "slug".to_owned()])
        );
    }

    #[test]
    fn extracts_placeholders_embedded_in_file_names() {
        assert_eq!(
            path_placeholders("posts/published-{slug}.md"),
            BTreeSet::from(["slug".to_owned()])
        );
        assert!(valid_path_template("posts/published-{slug}.md"));
        assert!(!valid_path_template("posts/{date}-{slug}.md"));
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        assert!(is_safe_relative("content/body.md"));
        assert!(!is_safe_relative("../secret"));
        assert!(!is_safe_relative("/absolute"));
    }
}
