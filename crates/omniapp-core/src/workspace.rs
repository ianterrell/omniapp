use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, NaiveDate};
use omniapp_schema::{
    Field, FieldSource, FieldType, Model, Problem, ProjectConfig, View, read_yaml, validate_config,
    validate_model, validate_view,
};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use walkdir::WalkDir;

use crate::{Cache, Record, RecordInput};

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("{0}")]
    Schema(#[from] omniapp_schema::SchemaError),
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("unknown model {0:?}")]
    UnknownModel(String),
    #[error("unknown record {key:?} for model {model:?}")]
    UnknownRecord { model: String, key: String },
    #[error("cache operation failed: {0}")]
    Cache(#[from] crate::cache::CacheError),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedWorkspace {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub models: BTreeMap<String, Model>,
    pub views: BTreeMap<String, View>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub diagnostics: Vec<Diagnostic>,
    pub models: usize,
    pub views: usize,
    pub records: usize,
}

impl ValidationReport {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

impl Workspace {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn metadata_dir(&self) -> PathBuf {
        self.root.join(".omniapp")
    }

    pub fn load(&self) -> Result<LoadedWorkspace, WorkspaceError> {
        let metadata = self.metadata_dir();
        if !metadata.is_dir() {
            return Err(WorkspaceError::Invalid(format!(
                "{} is not an OmniApp project (missing .omniapp directory)",
                self.root.display()
            )));
        }
        let config = read_yaml(&metadata.join("config.yml"))?;
        let models = load_named_yaml::<Model, _>(&metadata.join("models"), |model| &model.name)?;
        let views = load_named_yaml::<View, _>(&metadata.join("views"), |view| &view.name)?;
        Ok(LoadedWorkspace {
            root: self.root.clone(),
            config,
            models,
            views,
        })
    }

    pub fn records(&self, model: &Model) -> Result<Vec<Record>, WorkspaceError> {
        discover_record_dirs(&self.root, &model.storage.path)
            .into_iter()
            .map(|(directory, captures)| self.read_record(model, &directory, &captures))
            .collect()
    }

    pub fn all_records(&self, loaded: &LoadedWorkspace) -> Result<Vec<Record>, WorkspaceError> {
        let mut records = Vec::new();
        for model in loaded.models.values() {
            records.extend(self.records(model)?);
        }
        Ok(records)
    }

    pub fn validate(&self) -> Result<ValidationReport, WorkspaceError> {
        let loaded = self.load()?;
        let mut diagnostics = Vec::new();
        diagnostics.extend(problems_to_diagnostics(validate_config(&loaded.config)));
        for model in loaded.models.values() {
            diagnostics.extend(problems_to_diagnostics(validate_model(model)));
        }
        for view in loaded.views.values() {
            diagnostics.extend(problems_to_diagnostics(validate_view(view, &loaded.models)));
        }

        let mut all_records = Vec::new();
        for model in loaded.models.values() {
            match self.records(model) {
                Ok(records) => all_records.extend(records),
                Err(error) => diagnostics.push(Diagnostic::error(
                    format!("model {}", model.name),
                    error.to_string(),
                )),
            }
        }
        validate_records(&loaded.models, &all_records, &mut diagnostics);
        validate_references(&loaded.models, &all_records, &mut diagnostics);

        Ok(ValidationReport {
            models: loaded.models.len(),
            views: loaded.views.len(),
            records: all_records.len(),
            diagnostics,
        })
    }

    pub fn rebuild_cache(&self) -> Result<usize, WorkspaceError> {
        let loaded = self.load()?;
        let records = self.all_records(&loaded)?;
        fs::create_dir_all(self.metadata_dir())
            .map_err(|source| io_error(self.metadata_dir(), source))?;
        let mut cache = Cache::open(&self.metadata_dir().join("cache.sqlite3"))?;
        cache.rebuild(&records)?;
        Ok(records.len())
    }

    pub fn save_record(
        &self,
        model_name: &str,
        existing_key: Option<&str>,
        input: RecordInput,
    ) -> Result<Record, WorkspaceError> {
        let loaded = self.load()?;
        let model = loaded
            .models
            .get(model_name)
            .ok_or_else(|| WorkspaceError::UnknownModel(model_name.to_owned()))?;
        let model_problems = validate_model(model);
        if !model_problems.is_empty() {
            return Err(WorkspaceError::Invalid(
                model_problems
                    .into_iter()
                    .map(|problem| format!("{}: {}", problem.location, problem.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }

        let existing = if let Some(key) = existing_key {
            Some(
                self.records(model)?
                    .into_iter()
                    .find(|record| record.key == key)
                    .ok_or_else(|| WorkspaceError::UnknownRecord {
                        model: model_name.to_owned(),
                        key: key.to_owned(),
                    })?,
            )
        } else {
            None
        };

        let mut values = existing
            .as_ref()
            .map_or_else(BTreeMap::new, |record| record.values.clone());
        for (name, field) in &model.fields {
            if !values.contains_key(name)
                && let Some(default) = &field.default
            {
                values.insert(name.clone(), default.clone());
            }
        }
        for (name, value) in input.values {
            if !model.fields.contains_key(&name) {
                return Err(WorkspaceError::Invalid(format!(
                    "unknown field {name:?} on model {model_name}"
                )));
            }
            if value.is_null() {
                values.remove(&name);
            } else {
                values.insert(name, value);
            }
        }

        let target_relative = render_storage_path(model, &values)?;
        let target = self.root.join(&target_relative);
        let provisional = Record {
            key: record_key(&target_relative, &values),
            model: model.name.clone(),
            path: target_relative.clone(),
            values: values.clone(),
        };
        let mut validation = Vec::new();
        validate_record(model, &provisional, &mut validation);
        let mut candidate_records = self.all_records(&loaded)?;
        if let Some(existing) = &existing {
            candidate_records
                .retain(|record| record.model != existing.model || record.key != existing.key);
        }
        candidate_records.push(provisional.clone());
        validate_records(&loaded.models, &candidate_records, &mut validation);
        validate_references(&loaded.models, &candidate_records, &mut validation);
        validation.sort_by(|left, right| {
            (&left.location, &left.message).cmp(&(&right.location, &right.message))
        });
        validation.dedup_by(|left, right| {
            left.location == right.location && left.message == right.message
        });
        if !validation.is_empty() {
            return Err(WorkspaceError::Invalid(
                validation
                    .into_iter()
                    .map(|item| format!("{}: {}", item.location, item.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }

        if let Some(existing) = &existing {
            let old_path = self.root.join(&existing.path);
            if old_path != target {
                if target.exists() {
                    return Err(WorkspaceError::Invalid(format!(
                        "cannot move record to {}; it already exists",
                        target.display()
                    )));
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
                }
                fs::rename(&old_path, &target).map_err(|source| io_error(&old_path, source))?;
            }
        } else if target.exists() {
            return Err(WorkspaceError::Invalid(format!(
                "record directory {} already exists",
                target.display()
            )));
        }
        fs::create_dir_all(&target).map_err(|source| io_error(&target, source))?;
        write_record_files(&target, model, &values)?;

        let captures =
            match_storage_path(&model.storage.path, &target_relative).ok_or_else(|| {
                WorkspaceError::Invalid("rendered storage path did not match model".into())
            })?;
        self.read_record(model, &target, &captures)
    }

    pub fn delete_record(&self, model_name: &str, key: &str) -> Result<(), WorkspaceError> {
        let loaded = self.load()?;
        let model = loaded
            .models
            .get(model_name)
            .ok_or_else(|| WorkspaceError::UnknownModel(model_name.to_owned()))?;
        let record = self
            .records(model)?
            .into_iter()
            .find(|record| record.key == key)
            .ok_or_else(|| WorkspaceError::UnknownRecord {
                model: model_name.to_owned(),
                key: key.to_owned(),
            })?;
        let all_records = self.all_records(&loaded)?;
        for candidate in &all_records {
            if candidate.model != record.model && candidate.path.starts_with(&record.path) {
                return Err(WorkspaceError::Invalid(format!(
                    "cannot delete {}; nested {} record {:?} exists at {}",
                    record.path.display(),
                    candidate.model,
                    candidate.key,
                    candidate.path.display()
                )));
            }
            let Some(candidate_model) = loaded.models.get(&candidate.model) else {
                continue;
            };
            for (field_name, field) in &candidate_model.fields {
                let Some(reference) = &field.reference else {
                    continue;
                };
                if reference.model != record.model {
                    continue;
                }
                let Some(target_value) = record.values.get(&reference.field) else {
                    continue;
                };
                let Some(source_value) = candidate.values.get(field_name) else {
                    continue;
                };
                let points_here = if reference.many {
                    source_value
                        .as_array()
                        .is_some_and(|values| values.contains(target_value))
                } else {
                    source_value == target_value
                };
                if points_here {
                    return Err(WorkspaceError::Invalid(format!(
                        "cannot delete record; {} {:?} references it through field {field_name:?}",
                        candidate.model, candidate.key
                    )));
                }
            }
        }
        let path = self.root.join(record.path);
        fs::remove_dir_all(&path).map_err(|source| io_error(path, source))
    }

    fn read_record(
        &self,
        model: &Model,
        directory: &Path,
        captures: &BTreeMap<String, String>,
    ) -> Result<Record, WorkspaceError> {
        let relative = directory
            .strip_prefix(&self.root)
            .map_err(|_| WorkspaceError::Invalid("record escaped project root".into()))?
            .to_path_buf();
        let mut values = BTreeMap::new();
        let mut yaml_documents: HashMap<String, serde_yaml::Value> = HashMap::new();

        for (name, field) in &model.fields {
            let value = match &field.source {
                FieldSource::Path { variable } => captures
                    .get(variable)
                    .map(|value| Value::String(value.clone())),
                FieldSource::Yaml { file, key } => {
                    if !yaml_documents.contains_key(file) {
                        let path = directory.join(file);
                        let document = if path.exists() {
                            let contents = fs::read_to_string(&path)
                                .map_err(|source| io_error(&path, source))?;
                            serde_yaml::from_str(&contents).map_err(|error| {
                                WorkspaceError::Invalid(format!(
                                    "could not parse {}: {error}",
                                    path.display()
                                ))
                            })?
                        } else {
                            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
                        };
                        yaml_documents.insert(file.clone(), document);
                    }
                    yaml_documents
                        .get(file)
                        .and_then(serde_yaml::Value::as_mapping)
                        .and_then(|mapping| mapping.get(serde_yaml::Value::String(key.clone())))
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                }
                FieldSource::Markdown { file } => {
                    let path = directory.join(file);
                    path.exists()
                        .then(|| {
                            fs::read_to_string(&path)
                                .map(Value::String)
                                .map_err(|source| io_error(&path, source))
                        })
                        .transpose()?
                }
                FieldSource::Asset { file } => {
                    let path = directory.join(file);
                    path.exists()
                        .then(|| Value::String(relative.join(file).to_string_lossy().into_owned()))
                }
            };
            if let Some(value) = value.filter(|value| !value.is_null()) {
                values.insert(name.clone(), value);
            }
        }
        Ok(Record {
            key: record_key(&relative, &values),
            model: model.name.clone(),
            path: relative,
            values,
        })
    }
}

impl Diagnostic {
    fn error(location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            location: location.into(),
            message: message.into(),
        }
    }
}

fn load_named_yaml<T, F>(directory: &Path, name: F) -> Result<BTreeMap<String, T>, WorkspaceError>
where
    T: for<'de> serde::Deserialize<'de>,
    F: Fn(&T) -> &str,
{
    let mut values = BTreeMap::new();
    if !directory.exists() {
        return Ok(values);
    }
    let mut paths = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yml" | "yaml")
            )
        })
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let value: T = read_yaml(&path)?;
        let key = name(&value).to_owned();
        if values.insert(key.clone(), value).is_some() {
            return Err(WorkspaceError::Invalid(format!(
                "duplicate definition named {key:?}"
            )));
        }
    }
    Ok(values)
}

fn discover_record_dirs(root: &Path, template: &str) -> Vec<(PathBuf, BTreeMap<String, String>)> {
    let depth = template.split('/').count();
    let mut matches = WalkDir::new(root)
        .follow_links(false)
        .min_depth(depth)
        .max_depth(depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            match_storage_path(template, relative).map(|captures| (entry.into_path(), captures))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches
}

fn match_storage_path(template: &str, path: &Path) -> Option<BTreeMap<String, String>> {
    let template_parts = template.split('/').collect::<Vec<_>>();
    let path_parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if template_parts.len() != path_parts.len() {
        return None;
    }
    let mut captures = BTreeMap::new();
    for (expected, actual) in template_parts.into_iter().zip(path_parts) {
        if let Some(variable) = expected
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            captures.insert(variable.to_owned(), actual.to_owned());
        } else if expected != actual {
            return None;
        }
    }
    Some(captures)
}

fn render_storage_path(
    model: &Model,
    values: &BTreeMap<String, Value>,
) -> Result<PathBuf, WorkspaceError> {
    let mut path = PathBuf::new();
    for part in model.storage.path.split('/') {
        if let Some(variable) = part
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            let value = values
                .get(variable)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WorkspaceError::Invalid(format!("path field {variable:?} must be a string"))
                })?;
            if value.is_empty() || matches!(value, "." | "..") || value.contains(['/', '\\']) {
                return Err(WorkspaceError::Invalid(format!(
                    "path field {variable:?} must be one safe path segment"
                )));
            }
            path.push(value);
        } else {
            path.push(part);
        }
    }
    Ok(path)
}

fn record_key(path: &Path, values: &BTreeMap<String, Value>) -> String {
    values
        .get("id")
        .and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn write_record_files(
    directory: &Path,
    model: &Model,
    values: &BTreeMap<String, Value>,
) -> Result<(), WorkspaceError> {
    let mut yaml_files: BTreeMap<String, Vec<(&str, Option<&Value>)>> = BTreeMap::new();
    for (name, field) in &model.fields {
        match &field.source {
            FieldSource::Yaml { file, key } => {
                yaml_files
                    .entry(file.clone())
                    .or_default()
                    .push((key, values.get(name)));
            }
            FieldSource::Markdown { file } => {
                let path = directory.join(file);
                if let Some(Value::String(contents)) = values.get(name) {
                    atomic_write(&path, contents.as_bytes())?;
                } else if path.exists() {
                    fs::remove_file(&path).map_err(|source| io_error(path, source))?;
                }
            }
            FieldSource::Path { .. } | FieldSource::Asset { .. } => {}
        }
    }
    for (file, fields) in yaml_files {
        let path = directory.join(file);
        let mut mapping = if path.exists() {
            let contents = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
            serde_yaml::from_str::<serde_yaml::Value>(&contents)
                .map_err(|error| {
                    WorkspaceError::Invalid(format!("could not parse {}: {error}", path.display()))
                })?
                .as_mapping()
                .cloned()
                .ok_or_else(|| {
                    WorkspaceError::Invalid(format!(
                        "{} must contain a YAML mapping",
                        path.display()
                    ))
                })?
        } else {
            serde_yaml::Mapping::new()
        };
        for (key, value) in fields {
            let yaml_key = serde_yaml::Value::String(key.to_owned());
            if let Some(value) = value {
                let yaml_value = serde_yaml::to_value(value).map_err(|error| {
                    WorkspaceError::Invalid(format!("could not serialize {key}: {error}"))
                })?;
                mapping.insert(yaml_key, yaml_value);
            } else {
                mapping.remove(&yaml_key);
            }
        }
        let contents = serde_yaml::to_string(&mapping).map_err(|error| {
            WorkspaceError::Invalid(format!("could not serialize {}: {error}", path.display()))
        })?;
        atomic_write(&path, contents.as_bytes())?;
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), WorkspaceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    let temporary = path.with_extension(format!(
        "{}.omniapp-tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ));
    fs::write(&temporary, contents).map_err(|source| io_error(&temporary, source))?;
    fs::rename(&temporary, path).map_err(|source| io_error(path, source))
}

fn validate_records(
    models: &BTreeMap<String, Model>,
    records: &[Record],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut keys = BTreeSet::new();
    for record in records {
        if !keys.insert((record.model.clone(), record.key.clone())) {
            diagnostics.push(Diagnostic::error(
                record.path.display().to_string(),
                format!(
                    "duplicate record key {:?} for model {}",
                    record.key, record.model
                ),
            ));
        }
        if let Some(model) = models.get(&record.model) {
            validate_record(model, record, diagnostics);
        }
    }
}

fn validate_record(model: &Model, record: &Record, diagnostics: &mut Vec<Diagnostic>) {
    for (name, field) in &model.fields {
        let value = record.values.get(name);
        let location = format!("{}:{name}", record.path.display());
        if field.required && value.is_none_or(Value::is_null) {
            diagnostics.push(Diagnostic::error(&location, "required field is missing"));
            continue;
        }
        let Some(value) = value else { continue };
        validate_value(field, value, &location, diagnostics);
    }
}

fn validate_value(field: &Field, value: &Value, location: &str, diagnostics: &mut Vec<Diagnostic>) {
    let valid_type = match field.field_type {
        FieldType::String | FieldType::Text | FieldType::Enum | FieldType::Asset => {
            value.is_string()
        }
        FieldType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldType::Number => value.is_number(),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Date => value
            .as_str()
            .is_some_and(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()),
        FieldType::DateTime => value
            .as_str()
            .is_some_and(|value| DateTime::parse_from_rfc3339(value).is_ok()),
        FieldType::Reference => field.reference.as_ref().is_some_and(|reference| {
            if reference.many {
                value
                    .as_array()
                    .is_some_and(|values| values.iter().all(is_scalar))
            } else {
                is_scalar(value)
            }
        }),
        FieldType::Json => true,
    };
    if !valid_type {
        diagnostics.push(Diagnostic::error(
            location,
            format!("expected {:?}", field.field_type),
        ));
        return;
    }
    if !field.validation.choices.is_empty() && !field.validation.choices.contains(value) {
        diagnostics.push(Diagnostic::error(
            location,
            "value is not one of validation.choices",
        ));
    }
    if let Some(string) = value.as_str() {
        if field
            .validation
            .min_length
            .is_some_and(|min| string.chars().count() < min)
        {
            diagnostics.push(Diagnostic::error(
                location,
                "value is shorter than min_length",
            ));
        }
        if field
            .validation
            .max_length
            .is_some_and(|max| string.chars().count() > max)
        {
            diagnostics.push(Diagnostic::error(
                location,
                "value is longer than max_length",
            ));
        }
        if let Some(pattern) = &field.validation.pattern
            && let Ok(regex) = Regex::new(pattern)
            && !regex.is_match(string)
        {
            diagnostics.push(Diagnostic::error(location, "value does not match pattern"));
        }
    }
    if let Some(number) = value.as_f64() {
        if field.validation.min.is_some_and(|min| number < min) {
            diagnostics.push(Diagnostic::error(location, "value is less than min"));
        }
        if field.validation.max.is_some_and(|max| number > max) {
            diagnostics.push(Diagnostic::error(location, "value is greater than max"));
        }
    }
}

fn validate_references(
    models: &BTreeMap<String, Model>,
    records: &[Record],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for record in records {
        let Some(model) = models.get(&record.model) else {
            continue;
        };
        for (name, field) in &model.fields {
            let Some(reference) = &field.reference else {
                continue;
            };
            let Some(target_model) = models.get(&reference.model) else {
                diagnostics.push(Diagnostic::error(
                    format!("model {}.fields.{name}", model.name),
                    format!("references unknown model {:?}", reference.model),
                ));
                continue;
            };
            if !target_model.fields.contains_key(&reference.field) {
                diagnostics.push(Diagnostic::error(
                    format!("model {}.fields.{name}", model.name),
                    format!(
                        "references unknown field {:?}.{}",
                        reference.model, reference.field
                    ),
                ));
                continue;
            }
            let Some(value) = record.values.get(name) else {
                continue;
            };
            let values = if reference.many {
                value
                    .as_array()
                    .map_or_else(Vec::new, |values| values.iter().collect())
            } else {
                vec![value]
            };
            for value in values {
                let exists = records.iter().any(|candidate| {
                    candidate.model == reference.model
                        && candidate.values.get(&reference.field) == Some(value)
                });
                if !exists {
                    diagnostics.push(Diagnostic::error(
                        format!("{}:{name}", record.path.display()),
                        format!(
                            "reference {value} does not resolve to {}.{}",
                            reference.model, reference.field
                        ),
                    ));
                }
            }
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    value.is_string() || value.is_number()
}

fn problems_to_diagnostics(problems: Vec<Problem>) -> Vec<Diagnostic> {
    problems
        .into_iter()
        .map(|problem| Diagnostic::error(problem.location, problem.message))
        .collect()
}

fn io_error(path: impl AsRef<Path>, source: std::io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        path: path.as_ref().display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use omniapp_schema::{FieldSource, Storage, Validation};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn matches_nested_storage_templates() {
        let captures = match_storage_path(
            "books/{book}/scenes/{slug}",
            Path::new("books/war-and-peace/scenes/opening"),
        )
        .unwrap();
        assert_eq!(captures["book"], "war-and-peace");
        assert_eq!(captures["slug"], "opening");
    }

    #[test]
    fn writes_and_reads_mixed_source_record() {
        let directory = tempdir().unwrap();
        let workspace = Workspace::new(directory.path());
        fs::create_dir_all(directory.path().join(".omniapp/models")).unwrap();
        fs::create_dir_all(directory.path().join(".omniapp/views")).unwrap();
        fs::write(
            directory.path().join(".omniapp/config.yml"),
            "version: 1\nname: Test\n",
        )
        .unwrap();
        let model = Model {
            version: 1,
            name: "Book".into(),
            label: None,
            description: None,
            storage: Storage {
                path: "books/{slug}".into(),
            },
            fields: BTreeMap::from([
                (
                    "slug".into(),
                    field(
                        FieldType::String,
                        true,
                        FieldSource::Path {
                            variable: "slug".into(),
                        },
                    ),
                ),
                (
                    "title".into(),
                    field(
                        FieldType::String,
                        true,
                        FieldSource::Yaml {
                            file: "book.yml".into(),
                            key: "title".into(),
                        },
                    ),
                ),
                (
                    "body".into(),
                    field(
                        FieldType::Text,
                        false,
                        FieldSource::Markdown {
                            file: "README.md".into(),
                        },
                    ),
                ),
            ]),
            outputs: BTreeMap::new(),
        };
        fs::write(
            directory.path().join(".omniapp/models/book.yml"),
            serde_yaml::to_string(&model).unwrap(),
        )
        .unwrap();
        let record = workspace
            .save_record(
                "Book",
                None,
                RecordInput {
                    values: serde_json::from_value(
                        json!({"slug":"dune", "title":"Dune", "body":"# Dune\n"}),
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        assert_eq!(record.values["title"], "Dune");
        assert_eq!(
            fs::read_to_string(directory.path().join("books/dune/README.md")).unwrap(),
            "# Dune\n"
        );
        assert!(workspace.validate().unwrap().is_valid());
    }

    fn field(field_type: FieldType, required: bool, source: FieldSource) -> Field {
        Field {
            field_type,
            label: None,
            description: None,
            required,
            default: None,
            source,
            validation: Validation::default(),
            reference: None,
        }
    }
}
