//! The route table: expand pages (including per-record generator pages) into
//! output routes, detecting URL collisions and reserved-prefix use.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use omniapp_core::Record;
use omniapp_schema::{Query, View};

use crate::context::SiteData;
use crate::pages::{GeneratorSource, Page, PageKind};
use crate::{BuildProblem, SiteError};

/// URL path prefixes reserved for asset serving. Publishing a page under one
/// would diverge between build output and the dev server. The admin and API
/// live on their own port, so `admin/` and `api/` are NOT reserved.
const RESERVED_PREFIXES: [&str; 2] = ["assets", "files"];

/// What a matched route renders.
#[derive(Clone)]
pub(crate) enum RenderSpec {
    Page(usize),
    Record(usize, Arc<Record>),
}

#[derive(Clone)]
pub(crate) struct Route {
    pub url: String,
    pub output: PathBuf,
    pub render: RenderSpec,
}

pub(crate) struct RouteTable {
    pub routes: Vec<Route>,
    pub not_found: Option<usize>,
    /// Per-record permalink failures, surfaced as build problems.
    pub problems: Vec<BuildProblem>,
}

pub(crate) fn build_routes(
    pages: &[Page],
    views: &BTreeMap<String, View>,
    data: &Arc<SiteData>,
) -> Result<RouteTable, SiteError> {
    let mut routes = Vec::new();
    let mut not_found = None;
    let mut problems = Vec::new();

    for (index, page) in pages.iter().enumerate() {
        match &page.kind {
            PageKind::Static { url, output } => routes.push(Route {
                url: url.clone(),
                output: output.clone(),
                render: RenderSpec::Page(index),
            }),
            PageKind::NotFound => not_found = Some(index),
            PageKind::Generator(generator) => {
                let records = match &generator.source {
                    GeneratorSource::Model {
                        model,
                        filters,
                        order,
                    } => {
                        let query = Query {
                            filters: filters.clone(),
                            order: order.clone(),
                            page_size: 50,
                        };
                        data.query_records(model, &query)
                    }
                    GeneratorSource::View { view, model } => {
                        let query = views
                            .get(view)
                            .map(|view| view.query.clone())
                            .unwrap_or_default();
                        data.query_records(model, &query)
                    }
                };
                for record in records {
                    match generator.permalink.render(&record.values) {
                        Ok((url, output)) => routes.push(Route {
                            url,
                            output,
                            render: RenderSpec::Record(index, record),
                        }),
                        Err(error) => problems.push(BuildProblem {
                            location: format!("{} ({})", page.source_rel.display(), record.key),
                            message: error.to_string(),
                        }),
                    }
                }
            }
        }
    }

    detect_conflicts(pages, &routes)?;
    Ok(RouteTable {
        routes,
        not_found,
        problems,
    })
}

fn detect_conflicts(pages: &[Page], routes: &[Route]) -> Result<(), SiteError> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for route in routes {
        let source = route_source(pages, route);
        if let Some(segment) = route.url.trim_start_matches('/').split('/').next()
            && RESERVED_PREFIXES.contains(&segment)
        {
            return Err(SiteError::Reserved {
                url: route.url.clone(),
                origin: source,
            });
        }
        if let Some(first) = seen.insert(route.url.clone(), source.clone()) {
            return Err(SiteError::Collision {
                url: route.url.clone(),
                first,
                second: source,
            });
        }
    }
    Ok(())
}

fn route_source(pages: &[Page], route: &Route) -> String {
    match &route.render {
        RenderSpec::Page(index) => pages[*index].source_rel.display().to_string(),
        RenderSpec::Record(index, record) => {
            format!("{} ({})", pages[*index].source_rel.display(), record.key)
        }
    }
}
