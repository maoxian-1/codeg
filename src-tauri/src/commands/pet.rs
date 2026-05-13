//! Tauri + Axum command surface for the desktop-pet feature.
//!
//! All filesystem operations live in `crate::pets`; this module owns the
//! database-backed settings KV plus the thin double-mode wrappers that
//! translate raw I/O errors into `AppCommandError`. Window-management
//! commands live in `commands::windows::pet` to keep the Tauri-only code
//! together.

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::app_error::AppCommandError;
use crate::db::service::app_metadata_service;
#[cfg(feature = "tauri-runtime")]
use crate::db::AppDatabase;
use crate::models::pet::{
    ImportCodexPetsRequest, ImportCodexPetsResult, ImportablePet, NewPetInput, PetCelebrationKind,
    PetDetail, PetMetaPatch, PetSpriteAsset, PetState, PetSummary, PetWindowConfig,
    PetWindowStatePatch,
};
use crate::pet_state_mapper::{read_pet_state, PetStateHandle};
use crate::pets;
use crate::pets::marketplace::{
    MarketplaceInstallRequest, MarketplaceInstallResponse, MarketplaceListParams,
    MarketplaceListResponse,
};
use crate::web::event_bridge::{emit_event, EventEmitter};

/// KV key used by `app_metadata_service` for the persisted pet UI state.
const PET_CONFIG_KEY: &str = "pet.config";

// ─── pure ops (filesystem) ──────────────────────────────────────────────

pub async fn pet_list_core() -> Result<Vec<PetSummary>, AppCommandError> {
    tokio::task::spawn_blocking(pets::list_pets)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_get_core(id: String) -> Result<PetDetail, AppCommandError> {
    tokio::task::spawn_blocking(move || pets::get_pet(&id))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_read_spritesheet_core(id: String) -> Result<PetSpriteAsset, AppCommandError> {
    tokio::task::spawn_blocking(move || pets::read_pet_spritesheet(&id))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_add_core(input: NewPetInput) -> Result<PetSummary, AppCommandError> {
    tokio::task::spawn_blocking(move || pets::add_pet(input))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_update_meta_core(
    id: String,
    patch: PetMetaPatch,
) -> Result<PetSummary, AppCommandError> {
    tokio::task::spawn_blocking(move || pets::update_pet_meta(&id, patch))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_replace_sprite_core(
    id: String,
    spritesheet_base64: String,
) -> Result<(), AppCommandError> {
    tokio::task::spawn_blocking(move || pets::replace_pet_sprite(&id, &spritesheet_base64))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_delete_core(
    db: &DatabaseConnection,
    id: String,
) -> Result<(), AppCommandError> {
    let id_for_fs = id.clone();
    tokio::task::spawn_blocking(move || pets::delete_pet(&id_for_fs))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))??;

    // If the deleted pet was the active one, clear the active selection so
    // the renderer doesn't keep trying to load a missing asset.
    let mut config = load_config(db).await?;
    if config.active_pet_id.as_deref() == Some(&id) {
        config.active_pet_id = None;
        config.enabled = false;
        save_config(db, &config).await?;
    }
    Ok(())
}

pub async fn pet_list_importable_codex_core() -> Result<Vec<ImportablePet>, AppCommandError> {
    tokio::task::spawn_blocking(pets::codex_import::list_importable_codex_pets)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

pub async fn pet_import_codex_core(
    request: ImportCodexPetsRequest,
) -> Result<ImportCodexPetsResult, AppCommandError> {
    tokio::task::spawn_blocking(move || pets::codex_import::import_codex_pets(request))
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?
}

// ─── settings / window_state (DB-backed KV) ─────────────────────────────

async fn load_config(db: &DatabaseConnection) -> Result<PetWindowConfig, AppCommandError> {
    let raw = app_metadata_service::get_value(db, PET_CONFIG_KEY)
        .await
        .map_err(AppCommandError::db)?;
    let parsed = match raw {
        Some(s) => serde_json::from_str::<PetWindowConfig>(&s).unwrap_or_default(),
        None => PetWindowConfig::default(),
    };
    Ok(parsed)
}

async fn save_config(
    db: &DatabaseConnection,
    config: &PetWindowConfig,
) -> Result<(), AppCommandError> {
    let json = serde_json::to_string(config).map_err(|e| {
        AppCommandError::io_error(format!("Failed to serialize pet config: {e}"))
    })?;
    app_metadata_service::upsert_value(db, PET_CONFIG_KEY, &json)
        .await
        .map_err(AppCommandError::db)?;
    Ok(())
}

pub async fn pet_get_settings_core(
    db: &DatabaseConnection,
) -> Result<PetWindowConfig, AppCommandError> {
    load_config(db).await
}

pub async fn pet_set_active_core(
    db: &DatabaseConnection,
    emitter: &EventEmitter,
    pet_id: Option<String>,
) -> Result<PetWindowConfig, AppCommandError> {
    let mut config = load_config(db).await?;

    if let Some(ref id) = pet_id {
        // Defense in depth: don't persist a non-existent id.
        let id_clone = id.clone();
        let exists = tokio::task::spawn_blocking(move || pets::get_pet(&id_clone))
            .await
            .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
        let _ = exists?;
    }

    config.active_pet_id = pet_id;
    save_config(db, &config).await?;
    // Notify the live pet window (and any WebSocket subscribers) so it can
    // swap sprites in place rather than requiring close-and-reopen.
    emit_event(emitter, "pet://active-changed", &config);
    Ok(config)
}

/// Manual oneshot trigger for events the backend can't observe directly
/// (e.g. `folder://merge-completed`, which is currently emitted only by
/// the merge UI in the renderer). Goes through `emit_event` so both the
/// Tauri webview and any WebSocket clients see the same `pet://oneshot`
/// stream the mapper produces for ACP/git/install events. The narrowed
/// `PetCelebrationKind` keeps callers from broadcasting an ambient row
/// (e.g. `running`) — the frontend would silently drop those, which
/// makes the API quietly mis-behaved.
pub fn pet_celebrate_core(emitter: &EventEmitter, kind: PetCelebrationKind) {
    let state: crate::models::pet::PetState = kind.into();
    emit_event(emitter, "pet://oneshot", state);
}

/// Snapshot of the current ambient pet state. The mapper only emits
/// `pet://state` when the state changes, so a window that opens *after*
/// the agent already started prompting would otherwise sit on its default
/// `Idle` until the next ACP transition. The frontend calls this on mount
/// to fill in the gap.
pub fn pet_get_current_state_core(handle: &PetStateHandle) -> PetState {
    read_pet_state(handle)
}

pub async fn pet_save_window_state_core(
    db: &DatabaseConnection,
    patch: PetWindowStatePatch,
) -> Result<PetWindowConfig, AppCommandError> {
    let mut config = load_config(db).await?;
    if let Some(x) = patch.x {
        config.x = Some(x);
    }
    if let Some(y) = patch.y {
        config.y = Some(y);
    }
    if let Some(scale) = patch.scale {
        config.scale = scale.clamp(0.5, 3.0);
    }
    if let Some(top) = patch.always_on_top {
        config.always_on_top = top;
    }
    if let Some(enabled) = patch.enabled {
        config.enabled = enabled;
    }
    save_config(db, &config).await?;
    Ok(config)
}

// ─── Tauri command wrappers ─────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetIdParams {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetUpdateMetaParams {
    pub id: String,
    pub patch: PetMetaPatch,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetReplaceSpriteParams {
    pub id: String,
    pub spritesheet_base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetSetActiveParams {
    pub pet_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetCodexImportAvailability {
    pub available: bool,
}

pub async fn pet_codex_import_available_core() -> Result<PetCodexImportAvailability, AppCommandError>
{
    Ok(PetCodexImportAvailability {
        available: pets::codex_import::codex_import_available(),
    })
}

pub async fn pet_marketplace_list_core(
    params: MarketplaceListParams,
) -> Result<MarketplaceListResponse, AppCommandError> {
    pets::marketplace::list(params).await
}

pub async fn pet_marketplace_install_core(
    request: MarketplaceInstallRequest,
) -> Result<MarketplaceInstallResponse, AppCommandError> {
    pets::marketplace::install(request).await
}

// Tauri 2 looks up command parameters by their top-level name in the JSON
// args object. The frontend `lib/pet/api.ts` ships flat objects (e.g.
// `{ id, displayName, description, spritesheetBase64 }` for `pet_add`), so
// each command takes flat scalar parameters whose names match the camelCase
// keys after Tauri's auto snake_case translation. We *don't* declare a
// single struct param like `input: NewPetInput` — that would expect the
// frontend to wrap the payload as `{ input: { ... } }`, which it does not.

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_list() -> Result<Vec<PetSummary>, AppCommandError> {
    pet_list_core().await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_get(id: String) -> Result<PetDetail, AppCommandError> {
    pet_get_core(id).await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_read_spritesheet(id: String) -> Result<PetSpriteAsset, AppCommandError> {
    pet_read_spritesheet_core(id).await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_add(
    id: String,
    display_name: String,
    description: Option<String>,
    spritesheet_base64: String,
) -> Result<PetSummary, AppCommandError> {
    pet_add_core(NewPetInput {
        id,
        display_name,
        description,
        spritesheet_base64,
    })
    .await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_update_meta(
    id: String,
    patch: PetMetaPatch,
) -> Result<PetSummary, AppCommandError> {
    pet_update_meta_core(id, patch).await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_replace_sprite(
    id: String,
    spritesheet_base64: String,
) -> Result<(), AppCommandError> {
    pet_replace_sprite_core(id, spritesheet_base64).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_delete(
    db: tauri::State<'_, AppDatabase>,
    id: String,
) -> Result<(), AppCommandError> {
    pet_delete_core(&db.conn, id).await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_list_importable_codex() -> Result<Vec<ImportablePet>, AppCommandError> {
    pet_list_importable_codex_core().await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_import_codex(
    ids: Option<Vec<String>>,
    overwrite_with_suffix: Option<bool>,
) -> Result<ImportCodexPetsResult, AppCommandError> {
    pet_import_codex_core(ImportCodexPetsRequest {
        ids: ids.unwrap_or_default(),
        overwrite_with_suffix: overwrite_with_suffix.unwrap_or(false),
    })
    .await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_codex_import_available() -> Result<PetCodexImportAvailability, AppCommandError> {
    pet_codex_import_available_core().await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_marketplace_list(
    page: Option<u32>,
    page_size: Option<u32>,
    q: Option<String>,
    kind: Option<String>,
    sort: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<MarketplaceListResponse, AppCommandError> {
    pet_marketplace_list_core(MarketplaceListParams {
        page,
        page_size,
        q,
        kind,
        sort,
        tags,
    })
    .await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_marketplace_install(
    id: String,
    download_url: String,
    overwrite: Option<bool>,
) -> Result<MarketplaceInstallResponse, AppCommandError> {
    pet_marketplace_install_core(MarketplaceInstallRequest {
        id,
        download_url,
        overwrite: overwrite.unwrap_or(false),
    })
    .await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_get_settings(
    db: tauri::State<'_, AppDatabase>,
) -> Result<PetWindowConfig, AppCommandError> {
    pet_get_settings_core(&db.conn).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_set_active(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    pet_id: Option<String>,
) -> Result<PetWindowConfig, AppCommandError> {
    pet_set_active_core(&db.conn, &EventEmitter::Tauri(app), pet_id).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_celebrate(
    app: tauri::AppHandle,
    kind: PetCelebrationKind,
) -> Result<(), AppCommandError> {
    pet_celebrate_core(&EventEmitter::Tauri(app), kind);
    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_get_current_state(
    handle: tauri::State<'_, PetStateHandle>,
) -> Result<PetState, AppCommandError> {
    Ok(pet_get_current_state_core(handle.inner()))
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_save_window_state(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    x: Option<f64>,
    y: Option<f64>,
    scale: Option<f64>,
    always_on_top: Option<bool>,
    enabled: Option<bool>,
) -> Result<PetWindowConfig, AppCommandError> {
    let scale_changed = scale.is_some();
    let new_config = pet_save_window_state_core(
        &db.conn,
        PetWindowStatePatch {
            x,
            y,
            scale,
            always_on_top,
            enabled,
        },
    )
    .await?;

    // Keep the OS window in lockstep with the persisted scale. Without this,
    // changing scale via the right-click menu would shrink/grow the sprite
    // inside an unchanged transparent window — a 0.5x sprite floating in a
    // 1x window's worth of dead pixels that still capture clicks.
    if scale_changed {
        if let Some(window) = tauri::Manager::get_webview_window(&app, "pet") {
            let s = new_config.scale;
            let _ = window.set_size(tauri::LogicalSize::new(192.0_f64 * s, 208.0_f64 * s));
        }
    }

    Ok(new_config)
}
