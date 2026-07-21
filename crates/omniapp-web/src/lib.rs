//! Local-only HTTP API and schema-driven user interface.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Component;
use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use omniapp_core::{
    Cache, RecordInput, RecordsSnapshot, Workspace, WorkspaceError, WorkspaceWatcher,
};
use omniapp_schema::{Filter, Model, Query as RecordQuery, ViewType};
use omniapp_site::{LoadedSite, Resolution, SiteError, render_error_page};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower_http::services::ServeFile;
use tower_http::trace::TraceLayer;

const INDEX: &str = include_str!("index.html");
const APP_JS: &str = include_str!("app.js");
const APP_CSS: &str = include_str!("app.css");

/// Loaded public sites by name, each rebuilt lazily after the watcher
/// observes changes.
type SiteCaches = Arc<RwLock<HashMap<String, Arc<LoadedSite>>>>;

/// In-memory project snapshot for record read paths, loaded from the SQLite
/// cache on demand and cleared whenever the watcher observes a change.
type RecordsCache = Arc<RwLock<Option<Arc<RecordsSnapshot>>>>;

#[derive(Clone)]
struct AppState {
    workspace: Workspace,
    site_caches: SiteCaches,
    records_cache: RecordsCache,
}

/// Router state for one site's listener: the shared app state plus the name
/// of the site that listener serves.
#[derive(Clone)]
struct SiteState {
    app: AppState,
    site: String,
}

impl axum::extract::FromRef<SiteState> for AppState {
    fn from_ref(state: &SiteState) -> AppState {
        state.app.clone()
    }
}

impl AppState {
    /// The current snapshot, loading definitions and cached records if the
    /// watcher invalidated (or never populated) it.
    fn snapshot(&self) -> Result<Arc<RecordsSnapshot>, ApiError> {
        if let Some(snapshot) = self
            .records_cache
            .read()
            .ok()
            .and_then(|cached| cached.clone())
        {
            return Ok(snapshot);
        }
        let loaded = self.workspace.load()?;
        let cache = Cache::open(&self.workspace.metadata_dir().join("cache.sqlite3"))?;
        let snapshot = Arc::new(RecordsSnapshot {
            loaded,
            records: cache.all_records()?,
        });
        if let Ok(mut cached) = self.records_cache.write() {
            *cached = Some(Arc::clone(&snapshot));
        }
        Ok(snapshot)
    }

    /// Drop the snapshot after a mutation; the SQLite cache is already
    /// updated synchronously by `save_record`/`delete_record`, so the next
    /// request rebuilds a fresh snapshot without waiting for the watcher.
    fn invalidate(&self) {
        if let Ok(mut cached) = self.records_cache.write() {
            *cached = None;
        }
        if let Ok(mut cached) = self.site_caches.write() {
            cached.clear();
        }
    }
}

#[derive(Debug, Deserialize)]
struct PageParams {
    #[serde(default = "first_page")]
    page: usize,
    #[serde(default)]
    page_size: Option<usize>,
    /// Substring search across the whole result set (all pages).
    #[serde(default)]
    q: Option<String>,
    /// Extra filters applied on top of the view's query: a JSON filter
    /// object (`{"field":"book","op":"eq","value":"small"}`) or an array of
    /// them. Fields must exist on the model.
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KeyParams {
    key: String,
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn first_page() -> usize {
    1
}

fn default_search_limit() -> usize {
    50
}

/// The admin application and API, served at `/` on its own port.
fn admin_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/api/project", get(project))
        .route(
            "/api/models/{model}/records",
            get(model_records).post(create_record),
        )
        .route(
            "/api/models/{model}/record",
            get(get_record).put(update_record).delete(delete_record),
        )
        .route(
            "/api/models/{model}/record/relationships",
            get(record_relationships),
        )
        .route("/api/models/{model}/record/outputs", get(record_outputs))
        .route("/api/views/{view}/records", get(view_records))
        .route("/api/search", get(search))
        .route("/api/render/markdown", post(render_markdown))
        .route("/files/{*path}", get(project_asset))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// One public site's listener: live-rendered pages plus its own `/assets`
/// and the shared record `/files`.
fn site_router(state: SiteState) -> Router {
    Router::new()
        .route("/assets/{*path}", get(site_asset))
        .route("/files/{*path}", get(project_asset))
        .fallback(get(site_page))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn bind_available(starting_port: u16) -> std::io::Result<TcpListener> {
    for port in starting_port..=u16::MAX {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        match TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrNotAvailable,
        format!("no available port at or above {starting_port}"),
    ))
}

/// A set of bound listeners — one per site, plus optionally the admin — that
/// have not started serving yet, so callers can print the addresses first.
pub struct BoundServer {
    workspace: Workspace,
    sites: Vec<(String, SocketAddr, TcpListener)>,
    admin: Option<(SocketAddr, TcpListener)>,
}

impl BoundServer {
    #[must_use]
    pub fn site_addrs(&self) -> Vec<(String, SocketAddr)> {
        self.sites
            .iter()
            .map(|(name, addr, _)| (name.clone(), *addr))
            .collect()
    }

    #[must_use]
    pub fn admin_addr(&self) -> Option<SocketAddr> {
        self.admin.as_ref().map(|(addr, _)| *addr)
    }

    /// Start the watcher and serve every listener until one fails or the
    /// process is stopped.
    pub async fn serve(self) -> std::io::Result<()> {
        let state = AppState {
            workspace: self.workspace.clone(),
            site_caches: Arc::default(),
            records_cache: Arc::default(),
        };
        let invalidate = state.clone();
        let _watcher = WorkspaceWatcher::start_with_observer(
            self.workspace,
            Arc::new(move |_paths| invalidate.invalidate()),
        )
        .map_err(std::io::Error::other)?;

        let mut tasks = tokio::task::JoinSet::new();
        for (name, _, listener) in self.sites {
            let router = site_router(SiteState {
                app: state.clone(),
                site: name,
            });
            tasks.spawn(async move { axum::serve(listener, router).await });
        }
        if let Some((_, listener)) = self.admin {
            let router = admin_router(state.clone());
            tasks.spawn(async move { axum::serve(listener, router).await });
        }
        while let Some(finished) = tasks.join_next().await {
            finished.map_err(std::io::Error::other)??;
        }
        Ok(())
    }
}

/// Bind one port per site (in name order, starting at `starting_port`) and,
/// unless `with_admin` is false, one more for the admin application.
pub async fn bind(
    workspace: Workspace,
    starting_port: u16,
    with_admin: bool,
) -> std::io::Result<BoundServer> {
    let names = workspace.site_names().map_err(std::io::Error::other)?;
    let mut next_port = starting_port;
    let mut sites = Vec::new();
    for name in names {
        let listener = bind_available(next_port).await?;
        let addr = listener.local_addr()?;
        next_port = addr.port().saturating_add(1);
        sites.push((name, addr, listener));
    }
    let admin = if with_admin {
        let listener = bind_available(next_port).await?;
        let addr = listener.local_addr()?;
        Some((addr, listener))
    } else {
        None
    };
    Ok(BoundServer {
        workspace,
        sites,
        admin,
    })
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

/// Render a public-site page, loading (and caching) the named site on
/// demand. The watcher clears the cache on any project change, so edits
/// appear on the next request.
async fn site_page(State(state): State<SiteState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let cached = state
        .app
        .site_caches
        .read()
        .ok()
        .and_then(|cached| cached.get(&state.site).cloned());
    let site = if let Some(site) = cached {
        site
    } else {
        // Reuse the records snapshot instead of rescanning the filesystem:
        // cache reads are milliseconds even on large projects, so live edits
        // re-render almost instantly.
        let snapshot = match state.app.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return site_error_response(&SiteError::Invalid(error.message));
            }
        };
        match LoadedSite::load_with(
            &state.app.workspace,
            &state.site,
            &snapshot.loaded,
            snapshot.records.clone(),
        ) {
            Ok(site) => {
                let site = Arc::new(site);
                if let Ok(mut cached) = state.app.site_caches.write() {
                    cached.insert(state.site.clone(), Arc::clone(&site));
                }
                site
            }
            Err(error) => return site_error_response(&error),
        }
    };
    if !site.has_site() {
        // The site directory disappeared while serving.
        return (StatusCode::NOT_FOUND, Html(PLAIN_404.to_owned())).into_response();
    }
    match site.resolve(&path) {
        Ok(Resolution::Html(html)) => Html(html).into_response(),
        Ok(Resolution::Raw { content_type, body }) => {
            ([(header::CONTENT_TYPE, content_type)], body).into_response()
        }
        Ok(Resolution::Redirect(to)) => Redirect::temporary(&to).into_response(),
        Ok(Resolution::NotFound { html }) => (
            StatusCode::NOT_FOUND,
            Html(html.unwrap_or_else(|| PLAIN_404.to_owned())),
        )
            .into_response(),
        Err(error) => site_error_response(&error),
    }
}

fn site_error_response(error: &SiteError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html(render_error_page(error)),
    )
        .into_response()
}

const PLAIN_404: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Not found</title></head>\
<body style=\"font:16px/1.6 ui-sans-serif,system-ui,sans-serif;max-width:34rem;margin:18vh auto;padding:0 1.5rem;color:#18201d\">\
<h1 style=\"font:600 26px ui-serif,Georgia,serif\">Page not found</h1>\
<p><a href=\"/\" style=\"color:#245c47\">Back to the site</a></p></body></html>";

/// Serve a static site asset from `.omniapp/site/assets`.
async fn site_asset(
    State(state): State<SiteState>,
    Path(path): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let relative = std::path::Path::new(&path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ApiError::not_found("unknown site asset"));
    }
    let assets = state.app.workspace.site_dir(&state.site).join("assets");
    let root = std::fs::canonicalize(&assets)
        .map_err(|_| ApiError::not_found("this project has no site assets"))?;
    let absolute = std::fs::canonicalize(assets.join(relative))
        .map_err(|_| ApiError::not_found("site asset does not exist"))?;
    if !absolute.starts_with(&root) || !absolute.is_file() {
        return Err(ApiError::not_found("site asset does not exist"));
    }
    let response = ServeFile::new(absolute)
        .try_call(request)
        .await
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        })?;
    Ok(response.into_response())
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn app_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn project(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let loaded = state.workspace.load()?;
    Ok(Json(json!({
        "config": loaded.config,
        "models": loaded.models,
        "views": loaded.views,
    })))
}

/// The `filter` query param: one JSON filter object or an array of them,
/// with every field checked against the model.
fn parse_filters(raw: Option<&str>, model: &Model) -> Result<Vec<Filter>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Vec::new());
    };
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| ApiError::bad_request(format!("invalid filter: {error}")))?;
    let filters: Vec<Filter> = if value.is_array() {
        serde_json::from_value(value)
    } else {
        serde_json::from_value(value).map(|filter| vec![filter])
    }
    .map_err(|error| ApiError::bad_request(format!("invalid filter: {error}")))?;
    for filter in &filters {
        if !model.fields.contains_key(&filter.field) {
            return Err(ApiError::bad_request(format!(
                "unknown filter field {:?} on model {}",
                filter.field, model.name
            )));
        }
    }
    Ok(filters)
}

async fn model_records(
    State(state): State<AppState>,
    Path(model_name): Path<String>,
    Query(params): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    let loaded = state.workspace.load()?;
    let model = loaded
        .models
        .get(&model_name)
        .ok_or_else(|| WorkspaceError::UnknownModel(model_name.clone()))?;
    let query = RecordQuery {
        page_size: params.page_size.unwrap_or(50),
        filters: parse_filters(params.filter.as_deref(), model)?,
        ..RecordQuery::default()
    };
    let cache = Cache::open(&state.workspace.metadata_dir().join("cache.sqlite3"))?;
    Ok(Json(serde_json::to_value(cache.query_with_relations(
        &model.name,
        &loaded.models,
        &query,
        params.page,
        params.q.as_deref(),
    )?)?))
}

async fn view_records(
    State(state): State<AppState>,
    Path(view_name): Path<String>,
    Query(params): Query<PageParams>,
) -> Result<Json<Value>, ApiError> {
    let loaded = state.workspace.load()?;
    let view = loaded
        .views
        .get(&view_name)
        .ok_or_else(|| ApiError::not_found(format!("unknown view {view_name:?}")))?;
    let model = loaded
        .models
        .get(&view.model)
        .ok_or_else(|| WorkspaceError::UnknownModel(view.model.clone()))?;
    let mut query = view.query.clone();
    if let Some(page_size) = params.page_size {
        query.page_size = page_size;
    }
    query
        .filters
        .extend(parse_filters(params.filter.as_deref(), model)?);
    let cache = Cache::open(&state.workspace.metadata_dir().join("cache.sqlite3"))?;
    // A list view with `group_by` returns grouped buckets (with an optional
    // per-group limit) instead of a flat page; boards still group client-side.
    if let (ViewType::Table, Some(group_by)) = (view.view_type, view.group_by.as_deref()) {
        return Ok(Json(serde_json::to_value(cache.query_grouped(
            &model.name,
            &loaded.models,
            &query,
            group_by,
            view.group_limit,
            params.q.as_deref(),
        )?)?));
    }
    Ok(Json(serde_json::to_value(cache.query_with_relations(
        &model.name,
        &loaded.models,
        &query,
        params.page,
        params.q.as_deref(),
    )?)?))
}

async fn get_record(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Query(params): Query<KeyParams>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        state.snapshot()?.find_record(&model, &params.key)?,
    )?))
}

async fn create_record(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Json(input): Json<RecordInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let record = state.workspace.save_record(&model, None, input)?;
    state.invalidate();
    Ok((StatusCode::CREATED, Json(serde_json::to_value(record)?)))
}

async fn update_record(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Query(params): Query<KeyParams>,
    Json(input): Json<RecordInput>,
) -> Result<Json<Value>, ApiError> {
    let record = state
        .workspace
        .save_record(&model, Some(&params.key), input)?;
    state.invalidate();
    Ok(Json(serde_json::to_value(record)?))
}

async fn delete_record(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Query(params): Query<KeyParams>,
) -> Result<StatusCode, ApiError> {
    state
        .workspace
        .delete_record(&model, &params.key, params.revision.as_deref())?;
    state.invalidate();
    Ok(StatusCode::NO_CONTENT)
}

async fn record_relationships(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Query(params): Query<KeyParams>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        state.snapshot()?.relationships(&model, &params.key)?,
    )?))
}

async fn record_outputs(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Query(params): Query<KeyParams>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        state.snapshot()?.outputs(&model, &params.key)?,
    )?))
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    if params.q.trim().is_empty() {
        return Ok(Json(json!([])));
    }
    let cache = Cache::open(&state.workspace.metadata_dir().join("cache.sqlite3"))?;
    Ok(Json(serde_json::to_value(
        cache.search(&params.q, params.limit.clamp(1, 500))?,
    )?))
}

#[derive(Deserialize)]
struct MarkdownInput {
    texts: Vec<String>,
}

/// Batch-render markdown for the admin's `format: markdown` fields and
/// copy-as-HTML actions, using the same renderer as the generated sites.
async fn render_markdown(Json(input): Json<MarkdownInput>) -> Json<Value> {
    let html: Vec<String> = input
        .texts
        .iter()
        .map(|text| omniapp_site::render_markdown(text))
        .collect();
    Json(json!({ "html": html }))
}

async fn project_asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let relative = std::path::Path::new(&path);
    if !state.snapshot()?.is_known_asset(relative) {
        return Err(ApiError::not_found("unknown project asset"));
    }
    let root = std::fs::canonicalize(state.workspace.root()).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: error.to_string(),
    })?;
    let absolute = std::fs::canonicalize(state.workspace.root().join(relative))
        .map_err(|_| ApiError::not_found("asset file does not exist"))?;
    if !absolute.starts_with(&root) || !absolute.is_file() {
        return Err(ApiError::not_found("asset path is not a project file"));
    }
    let response = ServeFile::new(absolute)
        .try_call(request)
        .await
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        })?;
    Ok(response.into_response())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl From<WorkspaceError> for ApiError {
    fn from(error: WorkspaceError) -> Self {
        let status = match error {
            WorkspaceError::UnknownModel(_) | WorkspaceError::UnknownRecord { .. } => {
                StatusCode::NOT_FOUND
            }
            WorkspaceError::Conflict { .. } => StatusCode::CONFLICT,
            WorkspaceError::Invalid(_) | WorkspaceError::Schema(_) => StatusCode::BAD_REQUEST,
            WorkspaceError::Io { .. } | WorkspaceError::Cache(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl From<omniapp_core::CacheError> for ApiError {
    fn from(error: omniapp_core::CacheError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}
