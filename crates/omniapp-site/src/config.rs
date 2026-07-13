//! Loading and resolving `.omniapp/site/site.yml`.

use std::collections::BTreeMap;
use std::path::Path;

use omniapp_schema::{SiteConfig, validate_site_config};
use serde::Serialize;
use serde_json::Value;

use crate::SiteError;

/// Resolved site configuration with defaults applied.
#[derive(Debug, Clone)]
pub(crate) struct SiteSettings {
    pub title: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub params: BTreeMap<String, Value>,
}

impl SiteSettings {
    /// Load `site.yml` from the site directory, falling back to defaults when it
    /// is absent. `project_name` provides the title when neither the file nor a
    /// config value supplies one.
    pub(crate) fn load(site_dir: &Path, project_name: &str) -> Result<Self, SiteError> {
        let path = site_dir.join("site.yml");
        let config = if path.exists() {
            let config: SiteConfig = omniapp_schema::read_yaml(&path)
                .map_err(|error| SiteError::Workspace(error.into()))?;
            let problems = validate_site_config(&config);
            if !problems.is_empty() {
                return Err(SiteError::config("site.yml", &problems));
            }
            config
        } else {
            SiteConfig::default()
        };
        Ok(Self {
            title: config
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| project_name.to_owned()),
            description: config.description,
            url: config.url,
            params: config.params,
        })
    }

    /// Build the `site` template global for the given render timestamp.
    pub(crate) fn global(&self, time: &str) -> minijinja::Value {
        #[derive(Serialize)]
        struct SiteGlobal<'a> {
            title: &'a str,
            description: Option<&'a str>,
            url: Option<&'a str>,
            params: &'a BTreeMap<String, Value>,
            time: &'a str,
        }
        minijinja::Value::from_serialize(SiteGlobal {
            title: &self.title,
            description: self.description.as_deref(),
            url: self.url.as_deref(),
            params: &self.params,
            time,
        })
    }
}
