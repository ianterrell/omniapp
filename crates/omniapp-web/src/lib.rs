//! Local-only HTTP API and schema-driven user interface.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use omniapp_core::{Cache, RecordInput, Workspace, WorkspaceError, WorkspaceWatcher};
use omniapp_schema::Query as RecordQuery;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

const INDEX: &str = include_str!("index.html");

#[derive(Clone)]
struct AppState {
    workspace: Workspace,
}

#[derive(Debug, Deserialize)]
struct PageParams {
    #[serde(default = "first_page")]
    page: usize,
    #[serde(default)]
    page_size: Option<usize>,
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

pub fn router(workspace: Workspace) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/project", get(project))
        .route(
            "/api/models/{model}/records",
            get(model_records).post(create_record),
        )
        .route(
            "/api/models/{model}/record",
            put(update_record).delete(delete_record),
        )
        .route("/api/views/{view}/records", get(view_records))
        .route("/api/search", get(search))
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { workspace })
}

pub async fn bind_available(starting_port: u16) -> std::io::Result<TcpListener> {
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

pub async fn serve(workspace: Workspace, listener: TcpListener) -> std::io::Result<()> {
    let _watcher = WorkspaceWatcher::start(workspace.clone()).map_err(std::io::Error::other)?;
    axum::serve(listener, router(workspace)).await
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn project(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let loaded = state.workspace.load()?;
    Ok(Json(json!({
        "config": loaded.config,
        "models": loaded.models,
        "views": loaded.views,
    })))
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
        ..RecordQuery::default()
    };
    let cache = Cache::open(&state.workspace.metadata_dir().join("cache.sqlite3"))?;
    Ok(Json(serde_json::to_value(cache.query(
        &model.name,
        &query,
        params.page,
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
    let cache = Cache::open(&state.workspace.metadata_dir().join("cache.sqlite3"))?;
    Ok(Json(serde_json::to_value(cache.query(
        &model.name,
        &query,
        params.page,
    )?)?))
}

async fn create_record(
    State(state): State<AppState>,
    Path(model): Path<String>,
    Json(input): Json<RecordInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let record = state.workspace.save_record(&model, None, input)?;
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
    Ok(StatusCode::NO_CONTENT)
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
