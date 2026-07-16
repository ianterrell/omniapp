//! Declarative, stable-on-disk project format for OmniApp.

mod display;

pub use display::*;

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
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub navigation: Vec<NavItem>,
}

/// Base colors for the browser client; unset colors fall back to defaults and
/// all other shades are derived from these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub sidebar: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
}

/// One primary navigation entry: either a single view or a labelled group of
/// views rendered as tabs. Exactly one of `view`/`views` must be set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavItem {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub views: Vec<String>,
}

/// Configuration for the generated public site, stored at `.omniapp/site/site.yml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SiteConfig {
    pub version: u32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub params: BTreeMap<String, Value>,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            title: None,
            description: None,
            url: None,
            params: BTreeMap::new(),
        }
    }
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
    /// The reference field this model's records nest under. A nested model
    /// belongs to its parent record: the admin renders breadcrumbs through
    /// the parent chain, and the model is reached via its parent's pages
    /// rather than top-level navigation.
    #[serde(default)]
    pub parent: Option<String>,
    /// The field whose value titles a record wherever one is shown
    /// (breadcrumbs, page headers, cards, checklist rows). Without it,
    /// consumers fall back to a heuristic: title, then name, then the first
    /// required string field with a value, then the record key.
    #[serde(default)]
    pub title: Option<String>,
    /// URL segment for Rails-style nested admin routes. A root model renders
    /// at `#/<route>/<identity>`; a nested model (see `parent`) chains under
    /// its parent: `#/books/<book>/docs/<name>`.
    #[serde(default)]
    pub route: Option<String>,
    /// The field whose value identifies a record in URLs (unique among
    /// siblings). Records fall back to their key when unset.
    #[serde(default)]
    pub identity: Option<String>,
    /// Record-page sub-navigation. Each tab renders one of the model's
    /// display blocks in the record's context; the first tab is the default.
    #[serde(default)]
    pub tabs: Vec<Tab>,
    pub fields: BTreeMap<String, Field>,
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputSpec>,
    /// Named display blocks; `detail` lays out the record page.
    #[serde(default)]
    pub display: BTreeMap<String, DisplayBlock>,
}

/// One tab of a record page's sub-navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    pub label: String,
    /// The display block this tab renders ("detail" also allows the built-in
    /// default page when no such block is defined).
    pub block: String,
    /// URL segment for the tab; defaults to the block name. The first tab
    /// lives at the record's bare URL. A tab route may match a nested child
    /// model's route so the collection tab and member pages share a path.
    #[serde(default)]
    pub route: Option<String>,
}

impl Tab {
    #[must_use]
    pub fn route(&self) -> &str {
        self.route.as_deref().unwrap_or(&self.block)
    }
}

/// A named generated output: either a bare path template (a single file, the
/// common case) or a detailed form that can declare a directory whose actual
/// contents are enumerated at resolve time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OutputSpec {
    /// Shorthand: a path template resolving to one generated file.
    Path(String),
    /// Full form with an explicit kind.
    Detailed(OutputDetail),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDetail {
    pub path: String,
    #[serde(default)]
    pub kind: OutputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    #[default]
    File,
    Directory,
}

impl OutputSpec {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::Detailed(detail) => &detail.path,
        }
    }

    #[must_use]
    pub fn kind(&self) -> OutputKind {
        match self {
            Self::Path(_) => OutputKind::File,
            Self::Detailed(detail) => detail.kind,
        }
    }
}

impl From<&str> for OutputSpec {
    fn from(path: &str) -> Self {
        Self::Path(path.to_owned())
    }
}

impl From<String> for OutputSpec {
    fn from(path: String) -> Self {
        Self::Path(path)
    }
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
    pub display: Option<ViewDisplay>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewType {
    Form,
    Table,
    Tree,
    Board,
    Calendar,
    Cards,
    Timeline,
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
    for (name, color) in [
        ("accent", &config.theme.accent),
        ("sidebar", &config.theme.sidebar),
        ("background", &config.theme.background),
    ] {
        if let Some(color) = color
            && !is_hex_color(color)
        {
            problems.push(Problem::new(
                format!("config.theme.{name}"),
                format!("{color:?} must be a 6-digit hex color like \"#245c47\""),
            ));
        }
    }
    for (index, item) in config.navigation.iter().enumerate() {
        let location = format!("config.navigation[{index}]");
        match (&item.view, item.views.len()) {
            (Some(_), 0) => {}
            (None, count) if count > 0 => {
                if item
                    .label
                    .as_deref()
                    .is_none_or(|label| label.trim().is_empty())
                {
                    problems.push(Problem::new(&location, "view groups require a label"));
                }
            }
            (Some(_), _) => problems.push(Problem::new(
                &location,
                "set either view or views, not both",
            )),
            (None, _) => problems.push(Problem::new(
                &location,
                "requires a view or a non-empty views list",
            )),
        }
    }
    let mut seen = BTreeSet::new();
    for item in &config.navigation {
        for view in item.view.iter().chain(&item.views) {
            if !seen.insert(view.clone()) {
                problems.push(Problem::new(
                    "config.navigation",
                    format!("view {view:?} is listed more than once"),
                ));
            }
        }
    }
    problems
}

fn is_hex_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_route_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Cross-model checks for nested routes: root routes must be unique, a routed
/// child needs a routed parent chain, sibling routes must not collide, and the
/// child's back-reference must target the parent's identity field so URL
/// segments resolve records.
#[must_use]
pub fn validate_routes(models: &BTreeMap<String, Model>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut root_routes: BTreeMap<&str, &str> = BTreeMap::new();
    let mut child_routes: BTreeMap<(&str, &str), &str> = BTreeMap::new();
    for model in models.values() {
        let Some(route) = &model.route else { continue };
        let location = format!("model {}.route", model.name);
        let Some(parent_field) = &model.parent else {
            if let Some(other) = root_routes.insert(route, &model.name) {
                problems.push(Problem::new(
                    &location,
                    format!("route {route:?} is already used by model {other}"),
                ));
            }
            continue;
        };
        let Some(reference) = model
            .fields
            .get(parent_field)
            .and_then(|field| field.reference.as_ref())
        else {
            continue; // parent shape problems are reported by validate_model
        };
        let Some(parent) = models.get(&reference.model) else {
            continue;
        };
        if parent.route.is_none() {
            problems.push(Problem::new(
                &location,
                format!("nested route needs a route on parent model {}", parent.name),
            ));
        }
        if parent.identity.as_deref() != Some(reference.field.as_str()) {
            problems.push(Problem::new(
                &location,
                format!(
                    "parent model {} must declare identity: {} (the back-reference target) for URL segments to resolve",
                    parent.name, reference.field
                ),
            ));
        }
        if let Some(other) = child_routes.insert((reference.model.as_str(), route), &model.name) {
            problems.push(Problem::new(
                &location,
                format!(
                    "route {route:?} under {} is already used by model {other}",
                    reference.model
                ),
            ));
        }
    }
    problems
}

/// Validate navigation entries against the set of loaded views.
#[must_use]
pub fn validate_navigation(config: &ProjectConfig, views: &BTreeMap<String, View>) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (index, item) in config.navigation.iter().enumerate() {
        for view in item.view.iter().chain(&item.views) {
            if !views.contains_key(view) {
                problems.push(Problem::new(
                    format!("config.navigation[{index}]"),
                    format!("unknown view {view:?}"),
                ));
            }
        }
    }
    problems
}

#[must_use]
pub fn validate_site_config(config: &SiteConfig) -> Vec<Problem> {
    let mut problems = Vec::new();
    if config.version != FORMAT_VERSION {
        problems.push(Problem::new(
            "site.version",
            format!(
                "unsupported format version {}; expected {FORMAT_VERSION}",
                config.version
            ),
        ));
    }
    if let Some(url) = &config.url
        && !url.starts_with("http://")
        && !url.starts_with("https://")
    {
        problems.push(Problem::new("site.url", "must be an absolute http(s) URL"));
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
    if let Some(parent) = &model.parent {
        let is_reference = model
            .fields
            .get(parent)
            .is_some_and(|field| field.field_type == FieldType::Reference);
        if !is_reference {
            problems.push(Problem::new(
                format!("{location}.parent"),
                format!("parent {parent:?} must name a reference field"),
            ));
        }
    }
    if let Some(title) = &model.title
        && !model.fields.contains_key(title)
    {
        problems.push(Problem::new(
            format!("{location}.title"),
            format!("title {title:?} must name a field"),
        ));
    }
    if let Some(route) = &model.route {
        if !valid_route_segment(route) {
            problems.push(Problem::new(
                format!("{location}.route"),
                "route must be lowercase [a-z0-9-]+",
            ));
        }
        if matches!(route.as_str(), "records" | "views" | "edit" | "new") {
            problems.push(Problem::new(
                format!("{location}.route"),
                format!("route {route:?} is reserved"),
            ));
        }
    }
    if let Some(identity) = &model.identity
        && !model.fields.contains_key(identity)
    {
        problems.push(Problem::new(
            format!("{location}.identity"),
            format!("identity {identity:?} must name a field"),
        ));
    }
    let mut tab_routes = BTreeSet::new();
    for (index, tab) in model.tabs.iter().enumerate() {
        let tab_location = format!("{location}.tabs[{index}]");
        if tab.label.trim().is_empty() {
            problems.push(Problem::new(&tab_location, "label must not be empty"));
        }
        if tab.block != "detail" && !model.display.contains_key(&tab.block) {
            problems.push(Problem::new(
                &tab_location,
                format!("no display block {:?}", tab.block),
            ));
        }
        if !valid_route_segment(tab.route()) || matches!(tab.route(), "edit" | "new") {
            problems.push(Problem::new(
                &tab_location,
                "tab route must be lowercase [a-z0-9-]+ and not reserved",
            ));
        }
        if !tab_routes.insert(tab.route().to_owned()) {
            problems.push(Problem::new(
                &tab_location,
                format!("duplicate tab route {:?}", tab.route()),
            ));
        }
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
    for (name, spec) in &model.outputs {
        let path = spec.path();
        if !valid_output_template(path) || path.split('/').next() == Some(".omniapp") {
            problems.push(Problem::new(
                format!("{location}.outputs.{name}"),
                "must be a safe project-relative path template",
            ));
        }
        for placeholder in path_placeholders(path) {
            if !model.fields.contains_key(&placeholder) {
                problems.push(Problem::new(
                    format!("{location}.outputs.{name}"),
                    format!("references unknown field {placeholder:?}"),
                ));
            }
        }
    }
    problems.extend(validate_model_display(model));
    problems
}

/// A safe project-relative path template with `{field}` placeholders, as used
/// by model outputs and site permalinks.
#[must_use]
pub fn valid_output_template(template: &str) -> bool {
    if !is_safe_relative(template) {
        return false;
    }
    let mut remaining = template;
    while let Some(index) = remaining.find(['{', '}']) {
        if remaining.as_bytes()[index] == b'}' {
            return false;
        }
        let after_start = &remaining[index + 1..];
        let Some(end) = after_start.find('}') else {
            return false;
        };
        let field = &after_start[..end];
        if field.is_empty()
            || !field
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return false;
        }
        remaining = &after_start[end + 1..];
    }
    !remaining.contains('}')
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
    let check_query_field = |field: &str, suffix: &str, problems: &mut Vec<Problem>| {
        if let Err(message) = validate_query_field_path(model, field, models) {
            problems.push(Problem::new(format!("{location}.{suffix}"), message));
        }
    };
    for field in &view.fields {
        check_field(field, "fields", &mut problems);
    }
    for filter in &view.query.filters {
        check_query_field(&filter.field, "query.filters", &mut problems);
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
        check_query_field(&order.field, "query.order", &mut problems);
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
    if let Some(item) = view
        .display
        .as_ref()
        .and_then(|display| display.item.as_ref())
        && !model.display.contains_key(item)
    {
        problems.push(Problem::new(
            format!("{location}.display.item"),
            format!("model {} has no display block {item:?}", model.name),
        ));
    }
    problems
}

fn validate_query_field_path(
    root_model: &Model,
    field_path: &str,
    models: &BTreeMap<String, Model>,
) -> Result<(), String> {
    let mut model = root_model;
    let mut segments = field_path.split('.').peekable();
    while let Some(segment) = segments.next() {
        if segment.is_empty() {
            return Err(format!(
                "invalid empty segment in field path {field_path:?}"
            ));
        }
        let Some(field) = model.fields.get(segment) else {
            return Err(format!(
                "unknown field {segment:?} on model {} while resolving {field_path:?}",
                model.name
            ));
        };
        if segments.peek().is_some() {
            let Some(reference) = &field.reference else {
                return Err(format!(
                    "field {segment:?} on model {} is not a reference and cannot be traversed",
                    model.name
                ));
            };
            if reference.many {
                return Err(format!(
                    "many-valued reference {segment:?} on model {} cannot be traversed in a query",
                    model.name
                ));
            }
            model = models.get(&reference.model).ok_or_else(|| {
                format!(
                    "reference field {segment:?} points to unknown model {:?}",
                    reference.model
                )
            })?;
        }
    }
    if field_path.is_empty() {
        return Err("field path cannot be empty".to_owned());
    }
    Ok(())
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

    #[test]
    fn validates_nested_routes() {
        let book: Model = serde_yaml::from_str(
            r#"
version: 1
name: Book
storage: { kind: directory, path: "books/{slug}" }
route: books
identity: slug
fields:
  slug: { type: string, source: { kind: path, variable: slug } }
"#,
        )
        .unwrap();
        let doc: Model = serde_yaml::from_str(
            r#"
version: 1
name: BookDoc
storage: { kind: file, path: "books/{book}/docs/{name}.md" }
parent: book
route: docs
identity: name
fields:
  book:
    type: reference
    source: { kind: path, variable: book }
    reference: { model: Book, field: slug }
  name: { type: string, source: { kind: path, variable: name } }
"#,
        )
        .unwrap();
        let models = BTreeMap::from([
            ("Book".to_owned(), book.clone()),
            ("BookDoc".to_owned(), doc.clone()),
        ]);
        assert_eq!(validate_routes(&models), Vec::new());

        // Same root route on two models collides.
        let mut clash = book.clone();
        clash.name = "Novel".to_owned();
        let models = BTreeMap::from([
            ("Book".to_owned(), book.clone()),
            ("Novel".to_owned(), clash),
            ("BookDoc".to_owned(), doc.clone()),
        ]);
        assert!(
            validate_routes(&models)
                .iter()
                .any(|p| p.message.contains("already used"))
        );

        // A routed child needs the parent's identity to match its back-reference.
        let mut unkeyed = book;
        unkeyed.identity = None;
        let models = BTreeMap::from([("Book".to_owned(), unkeyed), ("BookDoc".to_owned(), doc)]);
        assert!(
            validate_routes(&models)
                .iter()
                .any(|p| p.message.contains("must declare identity"))
        );
    }

    #[test]
    fn validates_query_fields_through_references() {
        let book: Model = serde_yaml::from_str(
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
        let todo: Model = serde_yaml::from_str(
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
        let models = BTreeMap::from([("Book".to_owned(), book), ("Todo".to_owned(), todo)]);
        let valid: View = serde_yaml::from_str(
            r"
version: 1
name: active-todos
model: Todo
type: table
query:
  filters:
    - { field: book.publication_state, op: not_eq, value: archived }
",
        )
        .unwrap();
        assert_eq!(validate_view(&valid, &models), Vec::new());

        let invalid: View = serde_yaml::from_str(
            r"
version: 1
name: invalid
model: Todo
type: table
query:
  filters:
    - { field: id.publication_state, op: not_eq, value: archived }
",
        )
        .unwrap();
        assert!(
            validate_view(&invalid, &models)
                .iter()
                .any(|problem| { problem.message.contains("is not a reference") })
        );
    }

    fn config_with(theme: Theme, navigation: Vec<NavItem>) -> ProjectConfig {
        ProjectConfig {
            version: FORMAT_VERSION,
            name: "demo".to_owned(),
            description: None,
            theme,
            navigation,
        }
    }

    fn nav(label: Option<&str>, view: Option<&str>, views: &[&str]) -> NavItem {
        NavItem {
            label: label.map(str::to_owned),
            view: view.map(str::to_owned),
            views: views.iter().map(|v| (*v).to_owned()).collect(),
        }
    }

    #[test]
    fn accepts_valid_theme_and_navigation() {
        let config = config_with(
            Theme {
                accent: Some("#245c47".to_owned()),
                sidebar: Some("#17231E".to_owned()),
                background: None,
            },
            vec![
                nav(None, Some("posts"), &[]),
                nav(Some("Episodes"), None, &["episodes", "timeline"]),
            ],
        );
        assert!(validate_config(&config).is_empty());
    }

    #[test]
    fn rejects_malformed_theme_colors() {
        for bad in ["245c47", "#245c4", "#245c477", "#245c4g"] {
            let config = config_with(
                Theme {
                    accent: Some(bad.to_owned()),
                    sidebar: None,
                    background: None,
                },
                Vec::new(),
            );
            assert_eq!(
                validate_config(&config).len(),
                1,
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_malformed_navigation_items() {
        for (item, expected) in [
            (nav(None, Some("a"), &["b"]), "not both"),
            (nav(None, None, &[]), "requires a view"),
            (nav(None, None, &["a", "b"]), "require a label"),
        ] {
            let problems = validate_config(&config_with(Theme::default(), vec![item]));
            assert!(
                problems.iter().any(|p| p.message.contains(expected)),
                "expected {expected:?} in {problems:?}"
            );
        }
        let problems = validate_config(&config_with(
            Theme::default(),
            vec![nav(None, Some("a"), &[]), nav(Some("G"), None, &["a"])],
        ));
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("more than once"))
        );
    }

    #[test]
    fn navigation_views_must_exist() {
        let config = config_with(Theme::default(), vec![nav(None, Some("missing"), &[])]);
        let problems = validate_navigation(&config, &BTreeMap::new());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("missing"));
    }

    #[test]
    fn site_config_defaults_are_valid() {
        assert!(validate_site_config(&SiteConfig::default()).is_empty());
        let bad = SiteConfig {
            url: Some("ftp://example.com".to_owned()),
            ..SiteConfig::default()
        };
        assert_eq!(validate_site_config(&bad).len(), 1);
    }
}
