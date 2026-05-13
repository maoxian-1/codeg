use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::{extract::Extension, Json};
use serde::{Deserialize, Serialize};

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::web::{
    do_get_web_server_status, do_probe_web_service_port, do_stop_web_server,
    load_web_service_config, update_web_service_config_core, WebServerInfo, WebServiceConfig,
    WebServicePortProbe,
};

pub async fn get_web_server_status(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Option<WebServerInfo>>, AppCommandError> {
    Ok(Json(do_get_web_server_status(&state.web_server_state)))
}

pub async fn get_web_service_config(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<WebServiceConfig>, AppCommandError> {
    load_web_service_config(&state.db.conn).await.map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWebServiceConfigParams {
    pub config: WebServiceConfig,
}

pub async fn update_web_service_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<UpdateWebServiceConfigParams>,
) -> Result<Json<WebServiceConfig>, AppCommandError> {
    update_web_service_config_core(&state.db.conn, params.config)
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartWebServerParams {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub token: Option<String>,
}

pub async fn start_web_server(
    Extension(state): Extension<Arc<AppState>>,
    Json(_params): Json<StartWebServerParams>,
) -> Result<Json<WebServerInfo>, AppCommandError> {
    // In web mode, the server is already running (this handler itself is served by it).
    // This endpoint is mainly useful in Tauri mode. Return current status as a noop.
    let ws = &state.web_server_state;
    if ws.running.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(info) = do_get_web_server_status(ws) {
            return Ok(Json(info));
        }
    }
    Err(AppCommandError::new(
        crate::app_error::AppErrorCode::InvalidInput,
        "Cannot start web server from within web mode",
    ))
}

pub async fn stop_web_server(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<()>, AppCommandError> {
    // In web mode the serve task is owned by `codeg-server`'s main loop,
    // not WebServerState. Calling do_stop_web_server here would not stop
    // the process but WOULD trigger shutdown_signal — killing every live
    // WebSocket including the caller's own session. Reject instead.
    if state.web_server_state.is_externally_managed() {
        return Err(AppCommandError::new(
            crate::app_error::AppErrorCode::InvalidInput,
            "Cannot stop web server from within web mode",
        ));
    }
    do_stop_web_server(&state.web_server_state).await;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeWebServicePortParams {
    pub port: Option<u16>,
}

pub async fn probe_web_service_port(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ProbeWebServicePortParams>,
) -> Result<Json<WebServicePortProbe>, AppCommandError> {
    do_probe_web_service_port(&state.db.conn, params.port)
        .await
        .map(Json)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    pub version: String,
    pub body: String,
    pub date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheckResult {
    pub current_version: String,
    pub update: Option<AppUpdateInfo>,
}

#[derive(Deserialize)]
struct LatestManifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
}

// Mirrors the `endpoints` entry in `tauri.conf.json` so desktop and server
// modes consult the same source of truth.
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/xintaofei/codeg/releases/latest/download/latest.json";

// Built once on first use so we don't re-allocate the DNS resolver / TLS
// context for every settings-page mount. Proxy env vars are sampled here, so
// `init_proxy_from_db` must run before the first request — both startup paths
// already do that.
static UPDATE_HTTP_CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("codeg/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("failed to initialize update HTTP client: {e}"))
});

pub async fn check_app_update() -> Result<Json<AppUpdateCheckResult>, AppCommandError> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let manifest = fetch_latest_manifest().await?;

    let update = if is_newer_than(&manifest.version, &current_version) {
        Some(AppUpdateInfo {
            version: trim_v_prefix(&manifest.version).to_string(),
            body: manifest.notes.unwrap_or_default(),
            date: manifest.pub_date,
        })
    } else {
        None
    };

    Ok(Json(AppUpdateCheckResult {
        current_version,
        update,
    }))
}

async fn fetch_latest_manifest() -> Result<LatestManifest, AppCommandError> {
    let client = UPDATE_HTTP_CLIENT.as_ref().map_err(|err| {
        AppCommandError::network("Failed to initialize update HTTP client")
            .with_detail(err.clone())
    })?;

    let response = client
        .get(UPDATE_MANIFEST_URL)
        .send()
        .await
        .map_err(|e| {
            AppCommandError::network("Failed to fetch update manifest")
                .with_detail(e.to_string())
        })?;

    if !response.status().is_success() {
        return Err(AppCommandError::network(format!(
            "Update manifest returned status {}",
            response.status()
        )));
    }

    response.json::<LatestManifest>().await.map_err(|e| {
        AppCommandError::network("Failed to parse update manifest").with_detail(e.to_string())
    })
}

fn trim_v_prefix(v: &str) -> &str {
    v.strip_prefix('v').unwrap_or(v)
}

/// Best-effort semver comparison. Falls back to inequality if either side is
/// not a clean `X.Y.Z` triple — that way an unexpected manifest format still
/// surfaces *something* rather than silently claiming "already latest".
fn is_newer_than(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let core = trim_v_prefix(v).split(['-', '+']).next()?;
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() < 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    }
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => trim_v_prefix(latest) != trim_v_prefix(current),
    }
}
