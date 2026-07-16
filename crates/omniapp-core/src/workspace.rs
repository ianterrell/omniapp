use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use chrono::{DateTime, NaiveDate};
use omniapp_schema::{
    Field, FieldSource, FieldType, Model, Problem, ProjectConfig, Storage, View, is_safe_relative,
    read_yaml, validate_config, validate_display_references, validate_model, validate_navigation,
    validate_routes, validate_view,
};
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use walkdir::WalkDir;

use crate::document::MarkdownDocument;
use crate::yaml_edit::update_mapping;
use crate::{
    Cache, GeneratedOutput, OutputSet, Record, RecordInput, RelationshipLink, RelationshipSet,
};

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
    #[error("record {model} {key:?} changed since it was read; reload it before writing")]
    Conflict { model: String, key: String },
    #[error("cache operation failed: {0}")]
    Cache(#[from] crate::cache::CacheError),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

/// Cache metadata key holding the digest of the model definitions the cached
/// records were parsed under.
const MODELS_DIGEST_KEY: &str = "models_digest";

/// Path variables captured from a storage-template match.
type PathCaptures = BTreeMap<String, String>;

/// Discovered record locations per model name, each sorted by path.
type DiscoveredLocations = BTreeMap<String, Vec<(PathBuf, PathCaptures)>>;

/// A compiled storage-template segment: the regex and its variable names.
type SegmentMatcher = Option<(Regex, Vec<String>)>;

/// A digest of the model definitions; cached records parsed under a different
/// digest are stale regardless of their file fingerprints.
fn models_digest(models: &BTreeMap<String, Model>) -> String {
    let mut hasher = Sha256::new();
    for (name, model) in models {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_string(model).unwrap_or_default().as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedWorkspace {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub models: BTreeMap<String, Model>,
    pub views: BTreeMap<String, View>,
}

/// The result of reconciling the cache with the filesystem: the loaded
/// definitions plus every record, with only changed files re-read from disk.
#[derive(Debug)]
pub struct SyncedWorkspace {
    pub loaded: LoadedWorkspace,
    pub records: Vec<Record>,
    /// Records re-read because they were new or their fingerprint changed.
    pub refreshed: usize,
    /// Cache rows removed because their source files are gone or unreadable.
    pub removed: usize,
    /// Per-record read failures (the record is dropped from the cache).
    pub problems: Vec<Diagnostic>,
}

/// An in-memory view of the whole project — definitions plus every record —
/// for read paths that would otherwise rescan the filesystem per call.
/// Build one from [`Workspace::sync_cache`] results or cached records and
/// share it behind an `Arc`; the web server invalidates its copy whenever the
/// watcher reports a change.
#[derive(Debug)]
pub struct RecordsSnapshot {
    pub loaded: LoadedWorkspace,
    pub records: Vec<Record>,
}

impl RecordsSnapshot {
    /// Resolve a record by canonical key, storage path, or unique `id`/`slug`.
    pub fn find_record(&self, model_name: &str, selector: &str) -> Result<Record, WorkspaceError> {
        if !self.loaded.models.contains_key(model_name) {
            return Err(WorkspaceError::UnknownModel(model_name.to_owned()));
        }
        let records = self.model_records(model_name);
        find_in_records(&records, model_name, selector).map(|record| (*record).clone())
    }

    /// Outbound references and inbound backreferences for one record.
    pub fn relationships(
        &self,
        model_name: &str,
        key: &str,
    ) -> Result<RelationshipSet, WorkspaceError> {
        let model = self
            .loaded
            .models
            .get(model_name)
            .ok_or_else(|| WorkspaceError::UnknownModel(model_name.to_owned()))?;
        let same_model = self.model_records(model_name);
        let record = (*find_in_records(&same_model, model_name, key)?).clone();

        let mut outbound = Vec::new();
        for (field_name, field) in &model.fields {
            let Some(reference) = &field.reference else {
                continue;
            };
            let Some(value) = record.values.get(field_name) else {
                continue;
            };
            let values = relationship_values(value, reference.many);
            for target in self.records.iter().filter(|candidate| {
                candidate.model == reference.model
                    && candidate
                        .values
                        .get(&reference.field)
                        .is_some_and(|value| values.contains(&value))
            }) {
                outbound.push(RelationshipLink {
                    field: field_name.clone(),
                    target_field: reference.field.clone(),
                    record: target.clone(),
                });
            }
        }

        let mut inbound = Vec::new();
        for candidate in &self.records {
            let Some(candidate_model) = self.loaded.models.get(&candidate.model) else {
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
                if relationship_values(source_value, reference.many).contains(&target_value) {
                    inbound.push(RelationshipLink {
                        field: field_name.clone(),
                        target_field: reference.field.clone(),
                        record: candidate.clone(),
                    });
                }
            }
        }
        outbound.sort_by(|left, right| {
            (&left.field, &left.record.model, &left.record.key).cmp(&(
                &right.field,
                &right.record.model,
                &right.record.key,
            ))
        });
        inbound.sort_by(|left, right| {
            (&left.record.model, &left.field, &left.record.key).cmp(&(
                &right.record.model,
                &right.field,
                &right.record.key,
            ))
        });
        Ok(RelationshipSet {
            record,
            outbound,
            inbound,
        })
    }

    /// Declared generated-output paths for one record, with existence checks
    /// against the project root.
    pub fn outputs(&self, model_name: &str, key: &str) -> Result<OutputSet, WorkspaceError> {
        let model = self
            .loaded
            .models
            .get(model_name)
            .ok_or_else(|| WorkspaceError::UnknownModel(model_name.to_owned()))?;
        let records = self.model_records(model_name);
        let record = (*find_in_records(&records, model_name, key)?).clone();
        let mut outputs = Vec::new();
        for (name, spec) in &model.outputs {
            let path = render_path_template(spec.path(), &record.values)?;
            let absolute = self.loaded.root.join(&path);
            let is_directory = absolute.is_dir();
            let files = if spec.kind() == omniapp_schema::OutputKind::Directory && is_directory {
                list_output_files(&absolute)
            } else {
                Vec::new()
            };
            outputs.push(GeneratedOutput {
                name: name.clone(),
                path,
                kind: spec.kind(),
                exists: absolute.exists(),
                is_file: absolute.is_file(),
                is_directory,
                files,
            });
        }
        Ok(OutputSet { record, outputs })
    }

    /// Whether a project-relative path is the value of some record's asset
    /// field (and therefore safe to serve).
    #[must_use]
    pub fn is_known_asset(&self, path: &Path) -> bool {
        if path.is_absolute()
            || path.starts_with(".omniapp")
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return false;
        }
        self.records.iter().any(|record| {
            let Some(model) = self.loaded.models.get(&record.model) else {
                return false;
            };
            model.fields.iter().any(|(name, field)| {
                field.field_type == FieldType::Asset
                    && record
                        .values
                        .get(name)
                        .and_then(Value::as_str)
                        .is_some_and(|asset| Path::new(asset) == path)
            })
        })
    }

    fn model_records(&self, model_name: &str) -> Vec<&Record> {
        self.records
            .iter()
            .filter(|record| record.model == model_name)
            .collect()
    }
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

    /// Directory holding one source directory per public site.
    #[must_use]
    pub fn sites_dir(&self) -> PathBuf {
        self.metadata_dir().join("sites")
    }

    /// Source directory for one named public site.
    #[must_use]
    pub fn site_dir(&self, name: &str) -> PathBuf {
        self.sites_dir().join(name)
    }

    /// The project's site names: sorted subdirectories of `.omniapp/sites`.
    /// Names become output directories and printed URLs, so anything outside
    /// `[a-z0-9-]+` is rejected rather than silently skipped.
    pub fn site_names(&self) -> Result<Vec<String>, WorkspaceError> {
        let sites = self.sites_dir();
        if !sites.is_dir() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&sites).map_err(|source| io_error(&sites, source))? {
            let entry = entry.map_err(|source| io_error(&sites, source))?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(WorkspaceError::Invalid(format!(
                    "site directory name {name:?} is invalid; use lowercase letters, digits, and hyphens"
                )));
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
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
        discover_record_locations(&self.root, &model.storage)
            .into_iter()
            .map(|(location, captures)| self.read_record(model, &location, &captures))
            .collect()
    }

    pub fn all_records(&self, loaded: &LoadedWorkspace) -> Result<Vec<Record>, WorkspaceError> {
        let mut records = Vec::new();
        for (model_name, locations) in discover_all_record_locations(&self.root, &loaded.models) {
            let model = &loaded.models[&model_name];
            for (location, captures) in locations {
                records.push(self.read_record(model, &location, &captures)?);
            }
        }
        Ok(records)
    }

    /// Resolve a record by canonical key, storage path, or a unique `id`/`slug`
    /// value.
    pub fn find_record(&self, model_name: &str, selector: &str) -> Result<Record, WorkspaceError> {
        self.snapshot()?.find_record(model_name, selector)
    }

    /// Sync the cache and return an in-memory snapshot of the whole project.
    /// One-shot callers (CLI commands) get correct results after direct file
    /// edits; long-lived callers should cache the snapshot themselves and
    /// invalidate on watcher events.
    pub fn snapshot(&self) -> Result<RecordsSnapshot, WorkspaceError> {
        let synced = self.sync_cache()?;
        Ok(RecordsSnapshot {
            loaded: synced.loaded,
            records: synced.records,
        })
    }

    pub fn relationships(
        &self,
        model_name: &str,
        key: &str,
    ) -> Result<RelationshipSet, WorkspaceError> {
        self.snapshot()?.relationships(model_name, key)
    }

    pub fn outputs(&self, model_name: &str, key: &str) -> Result<OutputSet, WorkspaceError> {
        self.snapshot()?.outputs(model_name, key)
    }

    pub fn is_known_asset(&self, path: &Path) -> Result<bool, WorkspaceError> {
        Ok(self.snapshot()?.is_known_asset(path))
    }

    /// Validate the project after an incremental cache sync: only new or
    /// changed records are re-read from disk.
    pub fn validate(&self) -> Result<ValidationReport, WorkspaceError> {
        let synced = self.sync_cache()?;
        Ok(self.validate_synced(&synced))
    }

    /// Validate against an already-synced record set (avoids a second sync
    /// when the caller needs the records too, e.g. `serve`).
    #[must_use]
    pub fn validate_synced(&self, synced: &SyncedWorkspace) -> ValidationReport {
        validation_report(&synced.loaded, &synced.records, synced.problems.clone())
    }

    /// Validate every record freshly from disk, rebuilding the cache from the
    /// scan — the escape hatch when the fingerprint-based cache is suspected
    /// stale.
    pub fn validate_full(&self) -> Result<ValidationReport, WorkspaceError> {
        let loaded = self.load()?;
        let (pairs, problems) = self.scan_all(&loaded);
        let mut cache = self.open_cache()?;
        cache.rebuild(&pairs)?;
        cache.set_metadata(MODELS_DIGEST_KEY, &models_digest(&loaded.models))?;
        let records = pairs
            .into_iter()
            .map(|(record, _)| record)
            .collect::<Vec<_>>();
        Ok(validation_report(&loaded, &records, problems))
    }

    pub fn rebuild_cache(&self) -> Result<usize, WorkspaceError> {
        let loaded = self.load()?;
        let (records, _problems) = self.scan_all(&loaded);
        let mut cache = self.open_cache()?;
        cache.rebuild(&records)?;
        cache.set_metadata(MODELS_DIGEST_KEY, &models_digest(&loaded.models))?;
        Ok(records.len())
    }

    /// Read every record freshly from disk in parallel, collecting per-record
    /// failures instead of aborting the scan.
    fn scan_all(&self, loaded: &LoadedWorkspace) -> (Vec<(Record, String)>, Vec<Diagnostic>) {
        let mut work = Vec::new();
        for (model_name, locations) in discover_all_record_locations(&self.root, &loaded.models) {
            let model = &loaded.models[&model_name];
            for (location, captures) in locations {
                work.push((model, location, captures));
            }
        }
        let results = work
            .par_iter()
            .map(|(model, location, captures)| {
                let fingerprint = record_fingerprint(model, location);
                self.read_record(model, location, captures)
                    .map(|record| (record, fingerprint))
            })
            .collect::<Vec<_>>();
        let mut records = Vec::new();
        let mut problems = Vec::new();
        for ((model, _, _), result) in work.iter().zip(results) {
            match result {
                Ok(pair) => records.push(pair),
                Err(error) => problems.push(Diagnostic::error(
                    format!("model {}", model.name),
                    error.to_string(),
                )),
            }
        }
        (records, problems)
    }

    /// Reconcile the cache with the filesystem using stat fingerprints: one
    /// pruned walk discovers record locations, only new or changed records
    /// are re-read (in parallel), vanished rows are deleted, and everything
    /// else is returned straight from the cache. If the model definitions
    /// changed since the cache was written, every record is re-read.
    pub fn sync_cache(&self) -> Result<SyncedWorkspace, WorkspaceError> {
        let loaded = self.load()?;
        let mut cache = self.open_cache()?;
        let digest = models_digest(&loaded.models);
        let definitions_stale = cache.metadata(MODELS_DIGEST_KEY)? != Some(digest.clone());
        let mut cached = cache.fingerprints()?;

        let mut to_read = Vec::new();
        for (model_name, locations) in discover_all_record_locations(&self.root, &loaded.models) {
            let model = &loaded.models[&model_name];
            for (location, captures) in locations {
                let Ok(relative) = location.strip_prefix(&self.root) else {
                    continue;
                };
                let relative = relative.to_string_lossy().into_owned();
                let fingerprint = record_fingerprint(model, &location);
                let unchanged = cached
                    .remove(&(model_name.clone(), relative.clone()))
                    .is_some_and(|previous| previous == fingerprint);
                if unchanged && !definitions_stale {
                    continue;
                }
                to_read.push((model, location, captures, relative, fingerprint));
            }
        }
        // Rows left in `cached` have no matching file on disk any more.
        let mut removals: Vec<(String, String)> = cached.into_keys().collect();

        let mut upserts = Vec::new();
        let mut problems = Vec::new();
        let results = to_read
            .par_iter()
            .map(|(model, location, captures, _, _)| self.read_record(model, location, captures))
            .collect::<Vec<_>>();
        for ((model, _, _, relative, fingerprint), result) in to_read.into_iter().zip(results) {
            match result {
                Ok(record) => upserts.push((record, fingerprint)),
                Err(error) => {
                    problems.push(Diagnostic::error(
                        format!("model {}", model.name),
                        error.to_string(),
                    ));
                    removals.push((model.name.clone(), relative));
                }
            }
        }
        let refreshed = upserts.len();
        let removed = removals.len();
        cache.apply(&upserts, &removals)?;
        cache.set_metadata(MODELS_DIGEST_KEY, &digest)?;
        let records = cache.all_records()?;
        Ok(SyncedWorkspace {
            loaded,
            records,
            refreshed,
            removed,
            problems,
        })
    }

    /// Resolve a possibly absolute, possibly canonicalized path to its
    /// project-relative form, trying both the workspace root as given and its
    /// canonical form.
    fn project_relative(&self, path: &Path, canonical_root: Option<&Path>) -> Option<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        if let Ok(relative) = absolute.strip_prefix(&self.root) {
            return Some(relative.to_path_buf());
        }
        canonical_root.and_then(|root| absolute.strip_prefix(root).ok().map(Path::to_path_buf))
    }

    fn open_cache(&self) -> Result<Cache, WorkspaceError> {
        fs::create_dir_all(self.metadata_dir())
            .map_err(|source| io_error(self.metadata_dir(), source))?;
        Ok(Cache::open(&self.metadata_dir().join("cache.sqlite3"))?)
    }

    pub fn refresh_cache_paths(&self, paths: &[PathBuf]) -> Result<usize, WorkspaceError> {
        // Watcher events carry canonical absolute paths, which need not
        // textually contain the workspace root (a relative root like `.`, or
        // a symlinked ancestor such as Dropbox's macOS location), so resolve
        // against the canonical root too.
        let canonical_root = fs::canonicalize(&self.root).ok();
        let relative_paths = paths
            .iter()
            .filter_map(|path| self.project_relative(path, canonical_root.as_deref()))
            .collect::<Vec<_>>();
        if relative_paths.iter().any(|path| is_definition_path(path)) {
            return self.rebuild_cache();
        }
        let loaded = self.load()?;
        let mut affected = BTreeSet::new();
        for relative in &relative_paths {
            let relative = relative.as_path();
            if is_cache_path(relative) {
                continue;
            }
            for model in loaded.models.values() {
                let depth = model.storage.path().split('/').count();
                let candidate = match &model.storage {
                    Storage::File { .. } => relative.to_path_buf(),
                    Storage::Directory { .. } => relative.components().take(depth).collect(),
                };
                if match_storage_path(model.storage.path(), &candidate).is_some() {
                    affected.insert((model.name.clone(), candidate));
                }
            }
        }

        let mut cache = self.open_cache()?;
        for (model_name, relative) in &affected {
            let model = loaded
                .models
                .get(model_name)
                .ok_or_else(|| WorkspaceError::UnknownModel(model_name.clone()))?;
            cache.remove_path(model_name, relative)?;
            let location = self.root.join(relative);
            let exists = match &model.storage {
                Storage::Directory { .. } => location.is_dir(),
                Storage::File { .. } => location.is_file(),
            };
            if exists {
                let captures =
                    match_storage_path(model.storage.path(), relative).ok_or_else(|| {
                        WorkspaceError::Invalid(
                            "affected record no longer matches its model".into(),
                        )
                    })?;
                let record = self.read_record(model, &location, &captures)?;
                cache.upsert(&record, &record_fingerprint(model, &location))?;
            }
        }
        Ok(affected.len())
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
        if let Some(existing) = &existing
            && input.revision.as_deref() != Some(&existing.revision)
        {
            return Err(WorkspaceError::Conflict {
                model: model_name.to_owned(),
                key: existing.key.clone(),
            });
        }

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
            revision: existing
                .as_ref()
                .map_or_else(String::new, |record| record.revision.clone()),
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
        match &model.storage {
            Storage::Directory { .. } => {
                fs::create_dir_all(&target).map_err(|source| io_error(&target, source))?;
            }
            Storage::File { .. } => {
                let parent = target.parent().ok_or_else(|| {
                    WorkspaceError::Invalid("record file has no parent directory".into())
                })?;
                fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
            }
        }
        write_record_files(&target, model, &values)?;

        let captures =
            match_storage_path(model.storage.path(), &target_relative).ok_or_else(|| {
                WorkspaceError::Invalid("rendered storage path did not match model".into())
            })?;
        let record = self.read_record(model, &target, &captures)?;
        let mut cache = Cache::open(&self.metadata_dir().join("cache.sqlite3"))?;
        if let Some(existing) = &existing {
            cache.remove_path(model_name, &existing.path)?;
        }
        cache.upsert(&record, &record_fingerprint(model, &target))?;
        Ok(record)
    }

    pub fn delete_record(
        &self,
        model_name: &str,
        key: &str,
        expected_revision: Option<&str>,
    ) -> Result<(), WorkspaceError> {
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
        if expected_revision != Some(&record.revision) {
            return Err(WorkspaceError::Conflict {
                model: model_name.to_owned(),
                key: record.key.clone(),
            });
        }
        let all_records = self.all_records(&loaded)?;
        for candidate in &all_records {
            if matches!(&model.storage, Storage::Directory { .. })
                && candidate.model != record.model
                && candidate.path.starts_with(&record.path)
            {
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
        let path = self.root.join(&record.path);
        match &model.storage {
            Storage::Directory { .. } => {
                fs::remove_dir_all(&path).map_err(|source| io_error(&path, source))?;
            }
            Storage::File { .. } => {
                fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
            }
        }
        let mut cache = Cache::open(&self.metadata_dir().join("cache.sqlite3"))?;
        cache.remove_path(model_name, &record.path)?;
        Ok(())
    }

    fn read_record(
        &self,
        model: &Model,
        location: &Path,
        captures: &BTreeMap<String, String>,
    ) -> Result<Record, WorkspaceError> {
        let relative = location
            .strip_prefix(&self.root)
            .map_err(|_| WorkspaceError::Invalid("record escaped project root".into()))?
            .to_path_buf();
        let mut values = BTreeMap::new();
        let mut reader = SourceReader::default();
        let mut yaml_documents: HashMap<PathBuf, serde_yaml::Value> = HashMap::new();
        let mut markdown_documents: HashMap<PathBuf, (bool, MarkdownDocument)> = HashMap::new();

        for (name, field) in &model.fields {
            let value = match &field.source {
                FieldSource::Path { variable } => captures
                    .get(variable)
                    .map(|value| Value::String(value.clone())),
                FieldSource::Yaml { file, key } => {
                    let path = source_path(model, location, Some(file))?;
                    if !yaml_documents.contains_key(&path) {
                        let document = match reader.read_str(&path)? {
                            Some(contents) => serde_yaml::from_str(contents).map_err(|error| {
                                WorkspaceError::Invalid(format!(
                                    "could not parse {}: {error}",
                                    path.display()
                                ))
                            })?,
                            None => serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                        };
                        yaml_documents.insert(path.clone(), document);
                    }
                    yaml_documents
                        .get(&path)
                        .and_then(serde_yaml::Value::as_mapping)
                        .and_then(|mapping| mapping.get(serde_yaml::Value::String(key.clone())))
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                }
                FieldSource::Markdown { file } => {
                    let path = source_path(model, location, file.as_deref())?;
                    if !markdown_documents.contains_key(&path) {
                        let entry = match reader.read_str(&path)? {
                            Some(contents) => (true, MarkdownDocument::parse(contents, &path)?),
                            None => (false, MarkdownDocument::default()),
                        };
                        markdown_documents.insert(path.clone(), entry);
                    }
                    markdown_documents
                        .get(&path)
                        .filter(|(exists, _)| *exists)
                        .map(|(_, document)| Value::String(document.body.clone()))
                }
                FieldSource::Frontmatter { file, key } => {
                    let path = source_path(model, location, file.as_deref())?;
                    if !markdown_documents.contains_key(&path) {
                        let entry = match reader.read_str(&path)? {
                            Some(contents) => (true, MarkdownDocument::parse(contents, &path)?),
                            None => (false, MarkdownDocument::default()),
                        };
                        markdown_documents.insert(path.clone(), entry);
                    }
                    markdown_documents
                        .get(&path)
                        .and_then(|(_, document)| {
                            document
                                .frontmatter
                                .get(serde_yaml::Value::String(key.clone()))
                        })
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                }
                FieldSource::Asset { file } => {
                    let path = source_path(model, location, Some(file))?;
                    path.exists().then(|| {
                        Value::String(
                            path.strip_prefix(&self.root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .into_owned(),
                        )
                    })
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
            revision: record_revision(model, location, &mut reader)?,
            values,
        })
    }
}

/// Reads each record source file at most once, sharing the bytes between
/// field parsing and revision hashing.
#[derive(Default)]
struct SourceReader {
    files: HashMap<PathBuf, Option<Vec<u8>>>,
}

impl SourceReader {
    /// The file's contents, or `None` if it does not exist.
    fn read(&mut self, path: &Path) -> Result<Option<&[u8]>, WorkspaceError> {
        if !self.files.contains_key(path) {
            let contents = match fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(io_error(path, error)),
            };
            self.files.insert(path.to_path_buf(), contents);
        }
        Ok(self.files.get(path).and_then(Option::as_deref))
    }

    fn read_str(&mut self, path: &Path) -> Result<Option<&str>, WorkspaceError> {
        let owned = path.to_path_buf();
        match self.read(path)? {
            Some(bytes) => std::str::from_utf8(bytes).map(Some).map_err(|_| {
                io_error(
                    owned,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "stream did not contain valid UTF-8",
                    ),
                )
            }),
            None => Ok(None),
        }
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

fn discover_record_locations(
    root: &Path,
    storage: &Storage,
) -> Vec<(PathBuf, BTreeMap<String, String>)> {
    let template = storage.path();
    let depth = template.split('/').count();
    let mut matches = WalkDir::new(root)
        .follow_links(false)
        .min_depth(depth)
        .max_depth(depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| match storage {
            Storage::Directory { .. } => entry.file_type().is_dir(),
            Storage::File { .. } => entry.file_type().is_file(),
        })
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            match_storage_path(template, relative).map(|captures| (entry.into_path(), captures))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches
}

/// Discover record locations for every model in a single directory walk,
/// pruning subtrees that cannot prefix-match any storage template. Results are
/// keyed by model name with each list sorted by path, matching what
/// per-model discovery would have produced.
fn discover_all_record_locations(
    root: &Path,
    models: &BTreeMap<String, Model>,
) -> DiscoveredLocations {
    struct Target<'a> {
        model_name: &'a str,
        template: &'a str,
        depth: usize,
        wants_dir: bool,
    }
    let targets = models
        .values()
        .map(|model| Target {
            model_name: &model.name,
            template: model.storage.path(),
            depth: model.storage.path().split('/').count(),
            wants_dir: matches!(model.storage, Storage::Directory { .. }),
        })
        .collect::<Vec<_>>();
    let mut results: DiscoveredLocations = models
        .keys()
        .map(|name| (name.clone(), Vec::new()))
        .collect();
    let Some(max_depth) = targets.iter().map(|target| target.depth).max() else {
        return results;
    };
    let walker = WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|entry| {
            if !entry.file_type().is_dir() {
                return true;
            }
            let Ok(relative) = entry.path().strip_prefix(root) else {
                return true;
            };
            targets
                .iter()
                .any(|target| template_prefix_matches(target.template, relative))
        });
    for entry in walker.filter_map(Result::ok) {
        let Ok(relative) = entry.path().strip_prefix(root).map(Path::to_path_buf) else {
            continue;
        };
        let is_dir = entry.file_type().is_dir();
        for target in &targets {
            if target.depth != entry.depth() || target.wants_dir != is_dir {
                continue;
            }
            if let Some(captures) = match_storage_path(target.template, &relative) {
                results
                    .get_mut(target.model_name)
                    .expect("model result list was pre-inserted")
                    .push((entry.path().to_path_buf(), captures));
            }
        }
    }
    for locations in results.values_mut() {
        locations.sort_by(|left, right| left.0.cmp(&right.0));
    }
    results
}

/// Whether a directory's project-relative path could contain (or be) a record
/// location for the template: every existing component must match the
/// template's corresponding segment. Used to prune the discovery walk, so a
/// false negative would silently lose records — keep this permissive.
fn template_prefix_matches(template: &str, path: &Path) -> bool {
    let template_parts = template.split('/').collect::<Vec<_>>();
    let path_parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if path_parts.len() > template_parts.len() {
        return false;
    }
    template_parts
        .iter()
        .zip(path_parts)
        .all(|(expected, actual)| {
            segment_pattern(expected).is_some_and(|(pattern, _variables)| pattern.is_match(actual))
        })
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
        let (pattern, variables) = segment_pattern(expected)?;
        let matched = pattern.captures(actual)?;
        for (index, variable) in variables.iter().enumerate() {
            let value = matched.get(index + 1)?.as_str().to_owned();
            if let Some(previous) = captures.insert(variable.clone(), value.clone())
                && previous != value
            {
                return None;
            }
        }
    }
    Some(captures)
}

/// Segment patterns are matched against every walked directory entry, so
/// compiled regexes are memoized; the key space is the handful of distinct
/// segments in model storage templates.
fn segment_pattern(template: &str) -> Option<(Regex, Vec<String>)> {
    static PATTERNS: LazyLock<RwLock<HashMap<String, SegmentMatcher>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(cached) = PATTERNS.read().expect("segment pattern lock").get(template) {
        return cached.clone();
    }
    let compiled = compile_segment_pattern(template);
    PATTERNS
        .write()
        .expect("segment pattern lock")
        .insert(template.to_owned(), compiled.clone());
    compiled
}

fn compile_segment_pattern(template: &str) -> Option<(Regex, Vec<String>)> {
    let mut pattern = String::from("^");
    let mut variables = Vec::new();
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        pattern.push_str(&regex::escape(&remaining[..start]));
        let after_start = &remaining[start + 1..];
        let end = after_start.find('}')?;
        let variable = &after_start[..end];
        if variable.is_empty() {
            return None;
        }
        variables.push(variable.to_owned());
        pattern.push_str("(.+?)");
        remaining = &after_start[end + 1..];
    }
    if remaining.contains('}') {
        return None;
    }
    pattern.push_str(&regex::escape(remaining));
    pattern.push('$');
    Some((Regex::new(&pattern).ok()?, variables))
}

fn render_storage_path(
    model: &Model,
    values: &BTreeMap<String, Value>,
) -> Result<PathBuf, WorkspaceError> {
    let mut path = PathBuf::new();
    for part in model.storage.path().split('/') {
        path.push(render_template_segment(part, values)?);
    }
    Ok(path)
}

/// Find a record by canonical key, storage path, or a unique `id`/`slug` value.
/// The slice must contain records of a single model.
fn find_in_records<'a>(
    records: &[&'a Record],
    model_name: &str,
    selector: &str,
) -> Result<&'a Record, WorkspaceError> {
    if let Some(record) = records
        .iter()
        .copied()
        .find(|record| record.key == selector || record.path.to_string_lossy() == selector)
    {
        return Ok(record);
    }
    let matches = records
        .iter()
        .copied()
        .filter(|record| {
            ["id", "slug"].iter().any(|field| {
                record.values.get(*field).is_some_and(|value| match value {
                    Value::String(text) => text == selector,
                    Value::Number(number) => number.to_string() == selector,
                    _ => false,
                })
            })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [record] => Ok(record),
        [] => Err(WorkspaceError::UnknownRecord {
            model: model_name.to_owned(),
            key: selector.to_owned(),
        }),
        _ => Err(WorkspaceError::Invalid(format!(
            "record selector {selector:?} is ambiguous; use one of: {}",
            matches
                .iter()
                .map(|record| record.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Every file under a directory output, recursive, hidden entries skipped,
/// as sorted paths relative to the directory itself.
fn list_output_files(directory: &Path) -> Vec<String> {
    let hidden = |name: &std::ffi::OsStr| name.to_string_lossy().starts_with('.');
    let mut files: Vec<String> = WalkDir::new(directory)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| !hidden(entry.file_name()))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(directory)
                .ok()
                .map(|relative| relative.to_string_lossy().into_owned())
        })
        .collect();
    files.sort();
    files
}

/// Render a `{field}`-placeholder path template against record values, e.g. for
/// model outputs or site permalinks. Rendered values must be scalar and must
/// not introduce path separators or unsafe segments.
pub fn render_path_template(
    template: &str,
    values: &BTreeMap<String, Value>,
) -> Result<PathBuf, WorkspaceError> {
    let mut path = PathBuf::new();
    for segment in template.split('/') {
        let mut rendered = String::new();
        let mut remaining = segment;
        while let Some(start) = remaining.find('{') {
            rendered.push_str(&remaining[..start]);
            let after_start = &remaining[start + 1..];
            let end = after_start.find('}').ok_or_else(|| {
                WorkspaceError::Invalid(format!("invalid output template {template:?}"))
            })?;
            let field = &after_start[..end];
            let value = values.get(field).ok_or_else(|| {
                WorkspaceError::Invalid(format!(
                    "output template field {field:?} is missing from the record"
                ))
            })?;
            let value = match value {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                _ => {
                    return Err(WorkspaceError::Invalid(format!(
                        "output template field {field:?} must be a string or number"
                    )));
                }
            };
            if value.is_empty() || value.contains(['/', '\\']) {
                return Err(WorkspaceError::Invalid(format!(
                    "output template field {field:?} is not a safe path component"
                )));
            }
            rendered.push_str(&value);
            remaining = &after_start[end + 1..];
        }
        rendered.push_str(remaining);
        if matches!(rendered.as_str(), "" | "." | "..") {
            return Err(WorkspaceError::Invalid(format!(
                "output template rendered unsafe segment {rendered:?}"
            )));
        }
        path.push(rendered);
    }
    Ok(path)
}

fn render_template_segment(
    template: &str,
    values: &BTreeMap<String, Value>,
) -> Result<String, WorkspaceError> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        rendered.push_str(&remaining[..start]);
        let after_start = &remaining[start + 1..];
        let end = after_start.find('}').ok_or_else(|| {
            WorkspaceError::Invalid(format!("invalid path template segment {template:?}"))
        })?;
        let variable = &after_start[..end];
        let value = values
            .get(variable)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WorkspaceError::Invalid(format!("path field {variable:?} must be a string"))
            })?;
        if value.is_empty() || value.contains(['/', '\\']) {
            return Err(WorkspaceError::Invalid(format!(
                "path field {variable:?} must not be empty or contain path separators"
            )));
        }
        rendered.push_str(value);
        remaining = &after_start[end + 1..];
    }
    rendered.push_str(remaining);
    if matches!(rendered.as_str(), "" | "." | "..") {
        return Err(WorkspaceError::Invalid(format!(
            "rendered path segment {rendered:?} is unsafe"
        )));
    }
    Ok(rendered)
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
    location: &Path,
    model: &Model,
    values: &BTreeMap<String, Value>,
) -> Result<(), WorkspaceError> {
    #[derive(Default)]
    enum BodyUpdate {
        #[default]
        Unchanged,
        Remove,
        Set(String),
    }

    #[derive(Default)]
    struct DocumentUpdate {
        body: BodyUpdate,
        frontmatter: Vec<(String, Option<Value>)>,
    }

    let mut yaml_files: BTreeMap<PathBuf, Vec<(&str, Option<&Value>)>> = BTreeMap::new();
    let mut markdown_files: BTreeMap<PathBuf, DocumentUpdate> = BTreeMap::new();
    for (name, field) in &model.fields {
        match &field.source {
            FieldSource::Yaml { file, key } => {
                yaml_files
                    .entry(source_path(model, location, Some(file))?)
                    .or_default()
                    .push((key, values.get(name)));
            }
            FieldSource::Markdown { file } => {
                let path = source_path(model, location, file.as_deref())?;
                markdown_files.entry(path).or_default().body = values
                    .get(name)
                    .and_then(Value::as_str)
                    .map_or(BodyUpdate::Remove, |body| BodyUpdate::Set(body.to_owned()));
            }
            FieldSource::Frontmatter { file, key } => {
                let path = source_path(model, location, file.as_deref())?;
                markdown_files
                    .entry(path)
                    .or_default()
                    .frontmatter
                    .push((key.clone(), values.get(name).cloned()));
            }
            FieldSource::Path { .. } | FieldSource::Asset { .. } => {}
        }
    }
    for (path, fields) in yaml_files {
        let contents = if path.exists() {
            fs::read_to_string(&path).map_err(|source| io_error(&path, source))?
        } else {
            String::new()
        };
        let mut updates = Vec::new();
        for (key, value) in fields {
            let value = value
                .map(serde_yaml::to_value)
                .transpose()
                .map_err(|error| {
                    WorkspaceError::Invalid(format!("could not serialize {key}: {error}"))
                })?;
            updates.push((key.to_owned(), value));
        }
        let contents = update_mapping(&contents, &updates)?;
        atomic_write(&path, contents.as_bytes())?;
    }
    for (path, update) in markdown_files {
        let mut document = MarkdownDocument::read(&path)?;
        match update.body {
            BodyUpdate::Unchanged => {}
            BodyUpdate::Remove => document.body.clear(),
            BodyUpdate::Set(body) => document.body = body,
        }
        let has_configured_frontmatter = !update.frontmatter.is_empty();
        let frontmatter_updates = update
            .frontmatter
            .into_iter()
            .map(|(key, value)| {
                let value = value
                    .map(serde_yaml::to_value)
                    .transpose()
                    .map_err(|error| {
                        WorkspaceError::Invalid(format!("could not serialize {key}: {error}"))
                    })?;
                Ok((key, value))
            })
            .collect::<Result<Vec<_>, WorkspaceError>>()?;
        document.update_frontmatter(&frontmatter_updates)?;
        if matches!(&model.storage, Storage::Directory { .. })
            && document.body.is_empty()
            && document.frontmatter.is_empty()
        {
            if path.exists() {
                fs::remove_file(&path).map_err(|source| io_error(path, source))?;
            }
        } else {
            let contents = document.render(has_configured_frontmatter);
            atomic_write(&path, contents.as_bytes())?;
        }
    }
    Ok(())
}

fn source_path(
    model: &Model,
    location: &Path,
    configured_file: Option<&str>,
) -> Result<PathBuf, WorkspaceError> {
    match &model.storage {
        Storage::Directory { .. } => {
            configured_file
                .map(|file| location.join(file))
                .ok_or_else(|| {
                    WorkspaceError::Invalid("directory record source is missing its file".into())
                })
        }
        Storage::File { .. } => {
            if configured_file.is_some() {
                return Err(WorkspaceError::Invalid(
                    "single-file record sources must not name another file".into(),
                ));
            }
            Ok(location.to_path_buf())
        }
    }
}

/// The record's optimistic-concurrency token: a SHA-256 over each source
/// file's project-relative name and contents. The byte scheme (name, `0`,
/// contents, `0`, in `BTreeSet` order) must never change — stored revisions
/// are compared against freshly computed ones.
fn record_revision(
    model: &Model,
    location: &Path,
    reader: &mut SourceReader,
) -> Result<String, WorkspaceError> {
    let files = record_source_files(model, location, false);
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(location).unwrap_or(&path);
        hasher.update(relative.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        if let Some(contents) = reader.read(&path)? {
            hasher.update(contents);
        }
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// The files a record's identity depends on. Revisions hash only content
/// sources (`include_assets: false` — the historical revision file set, which
/// must not change); fingerprints also track asset files so an added or
/// removed asset invalidates the cached record.
fn record_source_files(model: &Model, location: &Path, include_assets: bool) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::new();
    match &model.storage {
        Storage::File { .. } => {
            files.insert(location.to_path_buf());
        }
        Storage::Directory { .. } => {
            for field in model.fields.values() {
                let file = match &field.source {
                    FieldSource::Yaml { file, .. } => Some(file.as_str()),
                    FieldSource::Markdown { file } | FieldSource::Frontmatter { file, .. } => {
                        file.as_deref()
                    }
                    FieldSource::Asset { file } => include_assets.then_some(file.as_str()),
                    FieldSource::Path { .. } => None,
                };
                if let Some(file) = file {
                    files.insert(location.join(file));
                }
            }
        }
    }
    files
}

/// A cheap stat-based change token for a record's source files: per file the
/// relative name with `mtime_ns:size`, or `absent`. Comparing stored and
/// fresh fingerprints decides whether a cached record needs re-reading.
fn record_fingerprint(model: &Model, location: &Path) -> String {
    let files = record_source_files(model, location, true);
    let mut parts = Vec::with_capacity(files.len());
    for path in files {
        let relative = path.strip_prefix(location).unwrap_or(&path);
        let state = match fs::metadata(&path) {
            Ok(metadata) => {
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_nanos());
                format!("{mtime}:{}", metadata.len())
            }
            Err(_) => "absent".to_owned(),
        };
        parts.push(format!("{}={state}", relative.to_string_lossy()));
    }
    parts.join(";")
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
    if field.field_type == FieldType::Asset
        && value
            .as_str()
            .is_some_and(|path| !is_safe_relative(path) || Path::new(path).starts_with(".omniapp"))
    {
        diagnostics.push(Diagnostic::error(
            location,
            "asset path must be a safe project-relative path outside .omniapp",
        ));
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
    let targets = models
        .values()
        .flat_map(|model| model.fields.values())
        .filter_map(|field| field.reference.as_ref())
        .map(|reference| (reference.model.clone(), reference.field.clone()))
        .collect::<BTreeSet<_>>();
    let mut target_values: HashSet<(&str, &str, String)> = HashSet::new();
    for (model_name, field_name) in &targets {
        let mut seen = BTreeMap::new();
        for record in records.iter().filter(|record| &record.model == model_name) {
            let Some(value) = record.values.get(field_name) else {
                continue;
            };
            let identity = value.to_string();
            target_values.insert((model_name.as_str(), field_name.as_str(), identity.clone()));
            if let Some(previous) = seen.insert(identity, record.path.clone()) {
                diagnostics.push(Diagnostic::error(
                    format!("model {model_name}.fields.{field_name}"),
                    format!(
                        "relationship target values must be unique; {} and {} have {value}",
                        previous.display(),
                        record.path.display()
                    ),
                ));
            }
        }
    }
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
                let exists = target_values.contains(&(
                    reference.model.as_str(),
                    reference.field.as_str(),
                    value.to_string(),
                ));
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

fn relationship_values(value: &Value, many: bool) -> Vec<&Value> {
    if many {
        value
            .as_array()
            .map_or_else(Vec::new, |values| values.iter().collect())
    } else {
        vec![value]
    }
}

/// Full project validation over an in-memory record set. `problems` seeds the
/// diagnostics with any per-record read failures from the scan or sync.
fn validation_report(
    loaded: &LoadedWorkspace,
    records: &[Record],
    problems: Vec<Diagnostic>,
) -> ValidationReport {
    let mut diagnostics = problems;
    diagnostics.extend(problems_to_diagnostics(validate_config(&loaded.config)));
    diagnostics.extend(problems_to_diagnostics(validate_navigation(
        &loaded.config,
        &loaded.views,
    )));
    for model in loaded.models.values() {
        diagnostics.extend(problems_to_diagnostics(validate_model(model)));
        diagnostics.extend(problems_to_diagnostics(validate_display_references(
            model,
            &loaded.models,
            &loaded.views,
        )));
    }
    for view in loaded.views.values() {
        diagnostics.extend(problems_to_diagnostics(validate_view(view, &loaded.models)));
    }
    diagnostics.extend(problems_to_diagnostics(validate_routes(&loaded.models)));
    validate_records(&loaded.models, records, &mut diagnostics);
    validate_references(&loaded.models, records, &mut diagnostics);
    ValidationReport {
        models: loaded.models.len(),
        views: loaded.views.len(),
        records: records.len(),
        diagnostics,
    }
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

/// Whether a project-relative path is part of the project definitions, whose
/// changes invalidate every cached record.
fn is_definition_path(relative: &Path) -> bool {
    relative == Path::new(".omniapp/config.yml")
        || relative.starts_with(".omniapp/models")
        || relative.starts_with(".omniapp/views")
}

fn is_cache_path(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("cache.sqlite3"))
        && relative.starts_with(".omniapp")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use omniapp_schema::{FieldSource, Reference, Storage, Validation};
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
    fn template_prefix_matching_prunes_only_impossible_directories() {
        let template = "series/{series}/books/{book}/beatsheets/{instance}/beats/{slug}.md";
        for viable in [
            "series",
            "series/dune",
            "series/dune/books",
            "series/dune/books/messiah/beatsheets/main",
        ] {
            assert!(
                template_prefix_matches(template, Path::new(viable)),
                "{viable} should stay walkable"
            );
        }
        for pruned in [
            "archive",
            "series/dune/docs",
            "series/dune/books/messiah/beatsheets/main/beats/opening.md/extra",
        ] {
            assert!(
                !template_prefix_matches(template, Path::new(pruned)),
                "{pruned} should be pruned"
            );
        }
        // A path exactly as deep as the template must match so directory
        // records are not pruned away.
        assert!(template_prefix_matches(
            "books/{slug}",
            Path::new("books/dune")
        ));
    }

    #[test]
    fn unified_discovery_matches_per_model_discovery() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        for path in [
            "books/dune/book.yml",
            "books/hyperion/book.yml",
            "notes/first.md",
            "unrelated/deep/tree/file.md",
        ] {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(absolute, "title: x\n").unwrap();
        }
        let book = Model {
            version: 1,
            name: "Book".into(),
            label: None,
            description: None,
            storage: Storage::Directory {
                path: "books/{slug}".into(),
            },
            fields: BTreeMap::new(),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        let note = Model {
            version: 1,
            name: "Note".into(),
            label: None,
            description: None,
            storage: Storage::File {
                path: "notes/{slug}.md".into(),
            },
            fields: BTreeMap::new(),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        let models = BTreeMap::from([("Book".to_owned(), book), ("Note".to_owned(), note)]);
        let unified = discover_all_record_locations(root, &models);
        for (name, model) in &models {
            let per_model = discover_record_locations(root, &model.storage);
            assert_eq!(unified[name], per_model, "discovery diverged for {name}");
        }
        assert_eq!(unified["Book"].len(), 2);
        assert_eq!(unified["Note"].len(), 1);
    }

    #[test]
    fn sync_cache_rereads_only_changed_records() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let model = Model {
            version: 1,
            name: "Note".into(),
            label: None,
            description: None,
            storage: Storage::File {
                path: "notes/{slug}.md".into(),
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
                        FieldSource::Frontmatter {
                            file: None,
                            key: "title".into(),
                        },
                    ),
                ),
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        write_test_project(root, &model);
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes/one.md"), "---\ntitle: One\n---\n").unwrap();
        fs::write(root.join("notes/two.md"), "---\ntitle: Two\n---\n").unwrap();
        let workspace = Workspace::new(root);

        let first = workspace.sync_cache().unwrap();
        assert_eq!(first.refreshed, 2);
        assert_eq!(first.records.len(), 2);

        // Nothing changed: nothing is re-read.
        let second = workspace.sync_cache().unwrap();
        assert_eq!(second.refreshed, 0);
        assert_eq!(second.removed, 0);
        assert_eq!(second.records.len(), 2);

        // Touch one file with new contents: only that record refreshes.
        fs::write(root.join("notes/one.md"), "---\ntitle: One updated\n---\n").unwrap();
        let third = workspace.sync_cache().unwrap();
        assert_eq!(third.refreshed, 1);
        let updated = third
            .records
            .iter()
            .find(|record| record.key == "notes/one.md")
            .unwrap();
        assert_eq!(updated.values["title"], serde_json::json!("One updated"));

        // Delete a file: its row disappears without touching the other.
        fs::remove_file(root.join("notes/two.md")).unwrap();
        let fourth = workspace.sync_cache().unwrap();
        assert_eq!(fourth.refreshed, 0);
        assert_eq!(fourth.removed, 1);
        assert_eq!(fourth.records.len(), 1);

        // Changing the model definitions invalidates every cached record.
        let mut changed = model.clone();
        changed.description = Some("changed".into());
        fs::write(
            root.join(".omniapp/models/model.yml"),
            serde_yaml::to_string(&changed).unwrap(),
        )
        .unwrap();
        let fifth = workspace.sync_cache().unwrap();
        assert_eq!(fifth.refreshed, 1);
    }

    #[test]
    fn record_revision_scheme_is_pinned() {
        // Revisions are optimistic-concurrency tokens compared against stored
        // values, so the hashing scheme must never drift. Expected digest is
        // sha256(relative_name . 0 . contents . 0) computed independently.
        let directory = tempdir().unwrap();
        let model = Model {
            version: 1,
            name: "Book".into(),
            label: None,
            description: None,
            storage: Storage::File {
                path: "books/{slug}.md".into(),
            },
            fields: BTreeMap::from([(
                "title".into(),
                field(
                    FieldType::String,
                    true,
                    FieldSource::Frontmatter {
                        file: None,
                        key: "title".into(),
                    },
                ),
            )]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        let location = directory.path().join("books/dune.md");
        fs::create_dir_all(location.parent().unwrap()).unwrap();
        fs::write(&location, "---\ntitle: Dune\n---\nDesert planet.\n").unwrap();
        let revision = record_revision(&model, &location, &mut SourceReader::default()).unwrap();
        assert_eq!(
            revision,
            "b9b4801775c40ec41cfb362b75617753f189777f28b1e27b19738b722b4e6d44"
        );
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
            storage: Storage::Directory {
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
                            file: Some("README.md".into()),
                        },
                    ),
                ),
                (
                    "cover".into(),
                    field(
                        FieldType::Asset,
                        false,
                        FieldSource::Asset {
                            file: "cover.jpg".into(),
                        },
                    ),
                ),
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::from([
                ("publication".into(), "build/{slug}/book-{slug}.pdf".into()),
                (
                    "artifacts".into(),
                    omniapp_schema::OutputSpec::Detailed(omniapp_schema::OutputDetail {
                        path: "build/{slug}".into(),
                        kind: omniapp_schema::OutputKind::Directory,
                    }),
                ),
            ]),
            display: BTreeMap::new(),
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
                    revision: None,
                    values: serde_json::from_value(
                        json!({"slug":"dune", "title":"Dune", "body":"# Dune\n"}),
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        assert_eq!(record.values["title"], "Dune");
        fs::write(
            directory.path().join("books/dune/cover.jpg"),
            b"large asset bytes",
        )
        .unwrap();
        assert!(
            workspace
                .is_known_asset(Path::new("books/dune/cover.jpg"))
                .unwrap()
        );
        assert!(
            !workspace
                .is_known_asset(Path::new(".omniapp/config.yml"))
                .unwrap()
        );
        assert_eq!(
            workspace.records(&model).unwrap()[0].revision,
            record.revision
        );
        let outputs = workspace.outputs("Book", &record.key).unwrap();
        let publication = |set: &OutputSet| {
            set.outputs
                .iter()
                .find(|output| output.name == "publication")
                .cloned()
                .unwrap()
        };
        assert_eq!(
            publication(&outputs).path,
            Path::new("build/dune/book-dune.pdf")
        );
        assert!(!publication(&outputs).exists);
        fs::create_dir_all(directory.path().join("build/dune/extra")).unwrap();
        fs::write(directory.path().join("build/dune/book-dune.pdf"), b"pdf").unwrap();
        fs::write(directory.path().join("build/dune/extra/notes.txt"), b"n").unwrap();
        fs::write(directory.path().join("build/dune/.hidden"), b"h").unwrap();
        let outputs = workspace.outputs("Book", &record.key).unwrap();
        assert!(publication(&outputs).is_file);
        let artifacts = outputs
            .outputs
            .iter()
            .find(|output| output.name == "artifacts")
            .unwrap();
        assert!(artifacts.is_directory);
        assert_eq!(
            artifacts.files,
            vec!["book-dune.pdf".to_owned(), "extra/notes.txt".to_owned()]
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("books/dune/README.md")).unwrap(),
            "# Dune\n"
        );
        let yaml_path = directory.path().join("books/dune/book.yml");
        fs::write(
            &yaml_path,
            "# book metadata\ntitle: \"Dune Messiah\" # display title\nexternal:\n  keep: true # untouched\n",
        )
        .unwrap();
        assert_eq!(
            workspace
                .refresh_cache_paths(std::slice::from_ref(&yaml_path))
                .unwrap(),
            1
        );
        let cache = Cache::open(&workspace.metadata_dir().join("cache.sqlite3")).unwrap();
        let cached = cache
            .query("Book", &omniapp_schema::Query::default(), 1, None)
            .unwrap();
        assert_eq!(cached.records[0].values["title"], "Dune Messiah");
        let current = workspace.records(&model).unwrap().remove(0);
        workspace
            .save_record(
                "Book",
                Some(&current.key),
                RecordInput {
                    revision: Some(current.revision),
                    values: serde_json::from_value(json!({"title":"Children of Dune"})).unwrap(),
                },
            )
            .unwrap();
        let preserved = fs::read_to_string(&yaml_path).unwrap();
        assert!(preserved.starts_with(
            "# book metadata\ntitle: Children of Dune # display title\nexternal:\n  keep: true # untouched\n"
        ));
        assert!(workspace.validate().unwrap().is_valid());
    }

    #[test]
    fn directory_record_uses_arbitrary_markdown_name_with_frontmatter() {
        let directory = tempdir().unwrap();
        let workspace = Workspace::new(directory.path());
        let model = Model {
            version: 1,
            name: "Article".into(),
            label: None,
            description: None,
            storage: Storage::Directory {
                path: "articles/{slug}".into(),
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
                        FieldSource::Frontmatter {
                            file: Some("manuscript.markdown".into()),
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
                            file: Some("manuscript.markdown".into()),
                        },
                    ),
                ),
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        write_test_project(directory.path(), &model);

        let record = workspace
            .save_record(
                "Article",
                None,
                RecordInput {
                    revision: None,
                    values: serde_json::from_value(
                        json!({"slug":"local-first", "title":"Local First", "body":"# Draft\n"}),
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        assert_eq!(record.values["body"], "# Draft\n");
        let contents = fs::read_to_string(
            directory
                .path()
                .join("articles/local-first/manuscript.markdown"),
        )
        .unwrap();
        assert!(contents.starts_with("---\ntitle: Local First\n---\n"));
        assert!(contents.ends_with("# Draft\n"));
        assert!(workspace.validate().unwrap().is_valid());
    }

    #[test]
    fn single_file_record_preserves_frontmatter_and_moves_with_path_fields() {
        let directory = tempdir().unwrap();
        let workspace = Workspace::new(directory.path());
        let model = Model {
            version: 1,
            name: "Post".into(),
            label: None,
            description: None,
            storage: Storage::File {
                path: "posts/{slug}.md".into(),
            },
            fields: BTreeMap::from([
                (
                    "date".into(),
                    field(
                        FieldType::Date,
                        true,
                        FieldSource::Frontmatter {
                            file: None,
                            key: "date".into(),
                        },
                    ),
                ),
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
                        FieldSource::Frontmatter {
                            file: None,
                            key: "title".into(),
                        },
                    ),
                ),
                (
                    "body".into(),
                    field(FieldType::Text, false, FieldSource::Markdown { file: None }),
                ),
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        write_test_project(directory.path(), &model);
        let created = workspace
            .save_record(
                "Post",
                None,
                RecordInput {
                    revision: None,
                    values: serde_json::from_value(json!({
                        "date":"2026-07-10",
                        "slug":"hello",
                        "title":"Hello",
                        "body":"First draft.\n"
                    }))
                    .unwrap(),
                },
            )
            .unwrap();
        let original = directory.path().join("posts/hello.md");
        let contents = fs::read_to_string(&original).unwrap();
        fs::write(
            &original,
            contents.replace(
                "title: Hello\n",
                "# title comment\ntitle: Hello # keep inline\nexternal: keep-me\n",
            ),
        )
        .unwrap();

        let stale = workspace.save_record(
            "Post",
            Some(&created.key),
            RecordInput {
                revision: Some(created.revision.clone()),
                values: serde_json::from_value(json!({"title":"Stale title"})).unwrap(),
            },
        );
        assert!(matches!(stale, Err(WorkspaceError::Conflict { .. })));
        let current = workspace.records(&model).unwrap().remove(0);

        let updated = workspace
            .save_record(
                "Post",
                Some(&created.key),
                RecordInput {
                    revision: Some(current.revision),
                    values: serde_json::from_value(json!({
                        "slug":"hello-world",
                        "title":"Hello World"
                    }))
                    .unwrap(),
                },
            )
            .unwrap();
        let moved = directory.path().join("posts/hello-world.md");
        assert!(!original.exists());
        assert!(moved.exists());
        let contents = fs::read_to_string(&moved).unwrap();
        assert!(contents.contains("title: Hello World"));
        assert!(contents.contains("# title comment\ntitle: Hello World # keep inline"));
        assert!(contents.contains("external: keep-me"));
        assert!(contents.ends_with("First draft.\n"));
        assert_eq!(updated.values["slug"], "hello-world");
        assert!(workspace.validate().unwrap().is_valid());

        workspace
            .delete_record("Post", &updated.key, Some(&updated.revision))
            .unwrap();
        assert!(!moved.exists());
    }

    #[test]
    fn resolves_outbound_relationships_and_inbound_backreferences() {
        let directory = tempdir().unwrap();
        let workspace = Workspace::new(directory.path());
        let book = Model {
            version: 1,
            name: "Book".into(),
            label: None,
            description: None,
            storage: Storage::Directory {
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
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        let mut book_reference = field(
            FieldType::Reference,
            true,
            FieldSource::Path {
                variable: "book".into(),
            },
        );
        book_reference.reference = Some(Reference {
            model: "Book".into(),
            field: "slug".into(),
            many: false,
        });
        let scene = Model {
            version: 1,
            name: "Scene".into(),
            label: None,
            description: None,
            storage: Storage::Directory {
                path: "books/{book}/scenes/{slug}".into(),
            },
            fields: BTreeMap::from([
                ("book".into(), book_reference),
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
                            file: "scene.yml".into(),
                            key: "title".into(),
                        },
                    ),
                ),
            ]),
            parent: None,
            title: None,
            route: None,
            identity: None,
            tabs: Vec::new(),
            outputs: BTreeMap::new(),
            display: BTreeMap::new(),
        };
        write_test_project(directory.path(), &book);
        fs::write(
            directory.path().join(".omniapp/models/scene.yml"),
            serde_yaml::to_string(&scene).unwrap(),
        )
        .unwrap();
        let book_record = workspace
            .save_record(
                "Book",
                None,
                RecordInput {
                    revision: None,
                    values: serde_json::from_value(json!({"slug":"dune", "title":"Dune"})).unwrap(),
                },
            )
            .unwrap();
        let scene_record = workspace
            .save_record(
                "Scene",
                None,
                RecordInput {
                    revision: None,
                    values: serde_json::from_value(json!({
                        "book":"dune", "slug":"arrival", "title":"Arrival"
                    }))
                    .unwrap(),
                },
            )
            .unwrap();

        let scene_links = workspace.relationships("Scene", &scene_record.key).unwrap();
        assert_eq!(scene_links.outbound.len(), 1);
        assert_eq!(scene_links.outbound[0].record.key, book_record.key);
        let book_links = workspace.relationships("Book", &book_record.key).unwrap();
        assert_eq!(book_links.inbound.len(), 1);
        assert_eq!(book_links.inbound[0].record.key, scene_record.key);
        assert!(workspace.validate().unwrap().is_valid());
    }

    fn write_test_project(root: &Path, model: &Model) {
        fs::create_dir_all(root.join(".omniapp/models")).unwrap();
        fs::create_dir_all(root.join(".omniapp/views")).unwrap();
        fs::write(root.join(".omniapp/config.yml"), "version: 1\nname: Test\n").unwrap();
        fs::write(
            root.join(".omniapp/models/model.yml"),
            serde_yaml::to_string(model).unwrap(),
        )
        .unwrap();
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
