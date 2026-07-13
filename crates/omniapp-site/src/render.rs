//! Rendering a single route to HTML, shared by the build pipeline and the dev
//! server's `resolve`.

use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::{Environment, Value};
use omniapp_schema::View;

use crate::SiteError;
use crate::config::SiteSettings;
use crate::context::{SiteData, record_object};
use crate::env::render_markdown;
use crate::pages::Page;
use crate::routes::{RenderSpec, Route};

/// Holds the constant template globals for a render pass so they are computed
/// once and shared across every route.
pub(crate) struct Renderer<'a> {
    data: &'a Arc<SiteData>,
    pages: &'a [Page],
    site: Value,
    records: Value,
    views: Value,
}

impl<'a> Renderer<'a> {
    pub(crate) fn new(
        data: &'a Arc<SiteData>,
        pages: &'a [Page],
        settings: &SiteSettings,
        views: &BTreeMap<String, View>,
        time: &str,
    ) -> Self {
        Self {
            data,
            pages,
            site: settings.global(time),
            records: data.records_global(),
            views: data.views_global(views),
        }
    }

    fn base(&self, page: &Page, url: &str, record: Option<Value>) -> Vec<(String, Value)> {
        let mut entries = vec![
            ("site".to_owned(), self.site.clone()),
            ("records".to_owned(), self.records.clone()),
            ("views".to_owned(), self.views.clone()),
            ("page".to_owned(), page_value(page, url)),
        ];
        if let Some(record) = record {
            entries.push(("record".to_owned(), record));
        }
        entries
    }

    pub(crate) fn render_route(
        &self,
        env: &mut Environment,
        route: &Route,
    ) -> Result<String, SiteError> {
        let (page_index, record) = match &route.render {
            RenderSpec::Page(index) => (*index, None),
            RenderSpec::Record(index, record) => {
                (*index, Some(record_object(self.data, record.clone())))
            }
        };
        let page = &self.pages[page_index];

        let body_context: Value = self
            .base(page, &route.url, record.clone())
            .into_iter()
            .collect();
        env.add_template_owned(page.template_name.clone(), page.body.clone())?;
        let mut rendered = env
            .get_template(&page.template_name)?
            .render(body_context)?;
        if page.is_markdown {
            rendered = render_markdown(&rendered);
        }
        if let Some(layout) = &page.layout {
            let mut entries = self.base(page, &route.url, record);
            entries.push(("content".to_owned(), Value::from_safe_string(rendered)));
            let name = format!("layouts/{layout}.html");
            let context: Value = entries.into_iter().collect();
            rendered = env.get_template(&name)?.render(context)?;
        }
        Ok(rendered)
    }
}

/// Build the `page` template value: all frontmatter keys plus the resolved
/// `url`, `path`, and `title`.
fn page_value(page: &Page, url: &str) -> Value {
    let mut entries: Vec<(String, Value)> = Vec::new();
    for (key, value) in &page.frontmatter {
        if let Some(key) = key.as_str() {
            entries.push((key.to_owned(), Value::from_serialize(value)));
        }
    }
    entries.push(("url".to_owned(), Value::from(url)));
    entries.push((
        "path".to_owned(),
        Value::from(page.source_rel.to_string_lossy().replace('\\', "/")),
    ));
    if let Some(title) = &page.title {
        entries.push(("title".to_owned(), Value::from(title.clone())));
    }
    entries.into_iter().collect()
}
