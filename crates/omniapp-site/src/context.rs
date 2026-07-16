//! Template data model: the shared [`SiteData`] index and the lazy record
//! objects exposed to minijinja templates.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use minijinja::Value;
use minijinja::value::{Enumerator, Object, ObjectRepr};
use omniapp_core::{Record, execute_query_all_with_relations, render_path_template};
use omniapp_schema::{Model, Query, Reference};

/// A parsed permalink template. The trailing slash decides whether the record
/// maps to a directory (`.../index.html`) or a bare file.
#[derive(Debug, Clone)]
pub(crate) struct Permalink {
    core: String,
    trailing: bool,
}

impl Permalink {
    pub(crate) fn parse(raw: &str) -> Self {
        Self {
            core: raw.trim_end_matches('/').to_owned(),
            trailing: raw.ends_with('/'),
        }
    }

    /// The template with any trailing slash removed, suitable for validation
    /// with [`omniapp_schema::valid_output_template`].
    pub(crate) fn template(&self) -> &str {
        &self.core
    }

    /// Render this permalink against a record's values into a `(url, output)`
    /// pair. `output` is the site-relative file path.
    pub(crate) fn render(
        &self,
        values: &BTreeMap<String, serde_json::Value>,
    ) -> Result<(String, PathBuf), omniapp_core::WorkspaceError> {
        let rendered = render_path_template(&self.core, values)?;
        let joined = rendered.to_string_lossy().replace('\\', "/");
        if self.trailing {
            Ok((
                format!("/{joined}/"),
                PathBuf::from(format!("{joined}/index.html")),
            ))
        } else {
            Ok((format!("/{joined}"), PathBuf::from(joined)))
        }
    }
}

/// A record model that has one or more inbound reference fields into another
/// model, indexed by the referenced ("target") model.
#[derive(Debug, Clone)]
struct InboundSource {
    model: String,
    field: String,
    target_field: String,
    many: bool,
}

/// Shared, immutable index over all records used for lazy attribute resolution.
#[derive(Debug)]
pub(crate) struct SiteData {
    models: BTreeMap<String, Model>,
    by_model: BTreeMap<String, Vec<Arc<Record>>>,
    permalinks: BTreeMap<String, Permalink>,
    inbound: BTreeMap<String, Vec<InboundSource>>,
}

impl SiteData {
    pub(crate) fn new(
        models: BTreeMap<String, Model>,
        records: Vec<Record>,
        permalinks: BTreeMap<String, Permalink>,
    ) -> Arc<Self> {
        let mut by_model: BTreeMap<String, Vec<Arc<Record>>> = BTreeMap::new();
        for record in records {
            by_model
                .entry(record.model.clone())
                .or_default()
                .push(Arc::new(record));
        }
        let mut inbound: BTreeMap<String, Vec<InboundSource>> = BTreeMap::new();
        for model in models.values() {
            for (field_name, field) in &model.fields {
                if let Some(reference) = &field.reference {
                    inbound
                        .entry(reference.model.clone())
                        .or_default()
                        .push(InboundSource {
                            model: model.name.clone(),
                            field: field_name.clone(),
                            target_field: reference.field.clone(),
                            many: reference.many,
                        });
                }
            }
        }
        Arc::new(Self {
            models,
            by_model,
            permalinks,
            inbound,
        })
    }

    pub(crate) fn models(&self) -> &BTreeMap<String, Model> {
        &self.models
    }

    /// All records of a model in discovery order.
    pub(crate) fn model_records(&self, model: &str) -> Vec<Arc<Record>> {
        self.by_model.get(model).cloned().unwrap_or_default()
    }

    /// Records of a model with a query's filters and ordering applied.
    pub(crate) fn query_records(&self, model: &str, query: &Query) -> Vec<Arc<Record>> {
        let owned: Vec<Record> = self
            .by_model
            .get(model)
            .map(|records| records.iter().map(|record| (**record).clone()).collect())
            .unwrap_or_default();
        let all_records = self
            .by_model
            .values()
            .flatten()
            .map(|record| (**record).clone())
            .collect::<Vec<_>>();
        execute_query_all_with_relations(&owned, &all_records, &self.models, query)
            .into_iter()
            .map(Arc::new)
            .collect()
    }

    /// The URL a record's generator page is published at, if any model targets
    /// it with a permalink.
    fn url_for(&self, record: &Record) -> Option<String> {
        let permalink = self.permalinks.get(&record.model)?;
        permalink.render(&record.values).ok().map(|(url, _)| url)
    }

    /// The `records` template global: model name -> list of record objects.
    pub(crate) fn records_global(self: &Arc<Self>) -> Value {
        self.by_model
            .iter()
            .map(|(model, records)| {
                let items = records
                    .iter()
                    .map(|record| record_object(self, record.clone()))
                    .collect::<Vec<_>>();
                (model.clone(), Value::from(items))
            })
            .collect()
    }

    /// The `views` template global: view name -> queried list of record objects.
    pub(crate) fn views_global(
        self: &Arc<Self>,
        views: &BTreeMap<String, omniapp_schema::View>,
    ) -> Value {
        views
            .iter()
            .map(|(name, view)| {
                let items = self
                    .query_records(&view.model, &view.query)
                    .into_iter()
                    .map(|record| record_object(self, record))
                    .collect::<Vec<_>>();
                (name.clone(), Value::from(items))
            })
            .collect()
    }

    /// Source records that reference `target` through `source.field`.
    fn inbound_records(&self, target: &Record, source: &InboundSource) -> Vec<Arc<Record>> {
        let Some(target_value) = target.values.get(&source.target_field) else {
            return Vec::new();
        };
        self.by_model
            .get(&source.model)
            .map(|records| {
                records
                    .iter()
                    .filter(|record| {
                        record.values.get(&source.field).is_some_and(|value| {
                            if source.many {
                                value
                                    .as_array()
                                    .is_some_and(|values| values.contains(target_value))
                            } else {
                                value == target_value
                            }
                        })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Convert a stored JSON value into a template value.
fn json_to_value(value: &serde_json::Value) -> Value {
    Value::from_serialize(value)
}

/// Build a lazy record object value backed by shared site data.
pub(crate) fn record_object(data: &Arc<SiteData>, record: Arc<Record>) -> Value {
    Value::from_object(RecordObject {
        data: data.clone(),
        record,
    })
}

#[derive(Debug)]
struct RecordObject {
    data: Arc<SiteData>,
    record: Arc<Record>,
}

impl RecordObject {
    fn resolve_reference(&self, name: &str, reference: &Reference) -> Value {
        let Some(raw) = self.record.values.get(name) else {
            return Value::UNDEFINED;
        };
        if reference.many {
            let items = raw
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|value| self.resolve_one(reference, value))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Value::from(items)
        } else {
            self.resolve_one(reference, raw)
        }
    }

    fn resolve_one(&self, reference: &Reference, value: &serde_json::Value) -> Value {
        self.data
            .by_model
            .get(&reference.model)
            .and_then(|records| {
                records
                    .iter()
                    .find(|record| record.values.get(&reference.field) == Some(value))
            })
            .map_or_else(
                || json_to_value(value),
                |record| record_object(&self.data, record.clone()),
            )
    }
}

impl Object for RecordObject {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let name = key.as_str()?;
        if let Some(model) = self.data.models.get(&self.record.model)
            && let Some(field) = model.fields.get(name)
        {
            if let Some(reference) = &field.reference {
                return Some(self.resolve_reference(name, reference));
            }
            return Some(
                self.record
                    .values
                    .get(name)
                    .map_or(Value::UNDEFINED, json_to_value),
            );
        }
        match name {
            "url" => self
                .data
                .url_for(&self.record)
                .map(|url| Value::from_safe_string(crate::env::encode_url_path(&url))),
            "inbound" => Some(Value::from_object(InboundObject {
                data: self.data.clone(),
                record: self.record.clone(),
            })),
            "meta" => Some(Value::from_object(MetaObject {
                record: self.record.clone(),
            })),
            "key" => Some(Value::from(self.record.key.clone())),
            "model" => Some(Value::from(self.record.model.clone())),
            _ => None,
        }
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        let names = self
            .data
            .models
            .get(&self.record.model)
            .map(|model| model.fields.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Enumerator::Iter(Box::new(names.into_iter().map(Value::from)))
    }
}

#[derive(Debug)]
struct InboundObject {
    data: Arc<SiteData>,
    record: Arc<Record>,
}

impl Object for InboundObject {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let name = key.as_str()?;
        let sources = self.data.inbound.get(&self.record.model)?;
        let source = if let Some((model, field)) = name.split_once('.') {
            sources
                .iter()
                .find(|source| source.model == model && source.field == field)?
        } else {
            let mut matching = sources.iter().filter(|source| source.model == name);
            let first = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            first
        };
        let items = self
            .data
            .inbound_records(&self.record, source)
            .into_iter()
            .map(|record| record_object(&self.data, record))
            .collect::<Vec<_>>();
        Some(Value::from(items))
    }
}

#[derive(Debug)]
struct MetaObject {
    record: Arc<Record>,
}

impl Object for MetaObject {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match key.as_str()? {
            "key" => Some(Value::from(self.record.key.clone())),
            "path" => Some(Value::from(self.record.path.to_string_lossy().into_owned())),
            "model" => Some(Value::from(self.record.model.clone())),
            _ => None,
        }
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Str(&["key", "path", "model"])
    }
}
