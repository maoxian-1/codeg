//! Background task that aggregates broadcaster events into two pet streams:
//!
//! * `pet://state` — the *ambient* `PetState` derived from cross-connection
//!   ACP signals (idle/waiting/running/review/failed). De-duplicated; only
//!   emitted when the computed state changes.
//! * `pet://oneshot` — *transient* feedback animations triggered by discrete
//!   events (turn_complete, git commit/push, merge abort, agent install,
//!   manual `pet_celebrate` calls). Always emitted; the renderer plays one
//!   loop and falls back to the current ambient state.
//!
//! Subscribes to the same broadcaster the lifecycle subscriber uses and
//! consumes multiple channels via a single `tokio::select!` loop.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::acp::types::{AcpEvent, ConnectionStatus, EventEnvelope};
use crate::db::entities::conversation::ConversationStatus;
use crate::models::pet::PetState;
use crate::web::event_bridge::{emit_event, EventEmitter, WebEvent, WebEventBroadcaster};

/// Shared latest-known `PetState`, written by the subscriber task and read
/// by the `pet_get_current_state` command. Lets a freshly opened pet window
/// pick up the *current* ambient state instead of waiting for the next
/// state transition — the subscriber only emits on changes, so without this
/// the frontend would otherwise sit on its default `Idle` indefinitely if
/// the agent was already running when the window opened.
pub type PetStateHandle = Arc<RwLock<PetState>>;

pub fn new_pet_state_handle() -> PetStateHandle {
    Arc::new(RwLock::new(PetState::Idle))
}

/// Read the current ambient state. Falls back to `Idle` if the lock is
/// poisoned — a poisoned lock means the writer panicked, in which case
/// the snapshot is stale and `Idle` is the safe default.
pub fn read_pet_state(handle: &PetStateHandle) -> PetState {
    handle.read().map(|guard| *guard).unwrap_or(PetState::Idle)
}

fn write_pet_state(handle: &PetStateHandle, value: PetState) {
    match handle.write() {
        Ok(mut guard) => *guard = value,
        Err(err) => {
            // A poisoned lock means a previous writer panicked. The handle
            // is now permanently stale, which would silently degrade the
            // open-pet-mid-conversation experience to "always Idle" with no
            // other symptom. Surface it so it shows up in operator logs.
            eprintln!("[Pet] pet_state handle poisoned, dropping write: {err}");
        }
    }
}

/// How long the ambient `Failed` state stays visible before automatically
/// fading back to whatever the rest of the snapshot would compute. Restarts
/// each time a fresh error event arrives.
const PET_FAILED_RECOVERY_MS: u64 = 4_000;

/// Aggregate snapshot of cross-connection ACP signals, derived from the
/// stream of `AcpEvent`s. Pure data — `compute_pet_state` is the sole
/// source of truth for translating it into a `PetState`.
#[derive(Debug, Clone, Default)]
pub struct PetGlobalState {
    /// Connections currently in `Prompting` (an in-flight prompt is streaming).
    prompting: HashSet<String>,
    /// Connections currently in `Connected` (idle but reachable).
    connected: HashSet<String>,
    /// Connections in a terminal `Error` state. We treat any error event as
    /// authoritative even if a later `StatusChanged` clears it — Codex's
    /// `failed` row should briefly play, then the next event will reset it.
    erroring: HashSet<String>,
    /// Outstanding permission requests (request_id → connection_id). The
    /// presence of *any* outstanding permission flips the state to Review.
    pending_permissions: HashMap<String, String>,
    /// Conversations in `PendingReview`. A turn ended with output the user
    /// hasn't acknowledged.
    pending_reviews: HashSet<i32>,
}

impl PetGlobalState {
    pub fn apply(&mut self, env: &EventEnvelope) {
        let conn = &env.connection_id;
        match &env.payload {
            AcpEvent::StatusChanged { status } => match status {
                ConnectionStatus::Prompting => {
                    self.prompting.insert(conn.clone());
                    self.connected.insert(conn.clone());
                    self.erroring.remove(conn);
                }
                ConnectionStatus::Connected | ConnectionStatus::Connecting => {
                    self.prompting.remove(conn);
                    self.connected.insert(conn.clone());
                    self.erroring.remove(conn);
                }
                ConnectionStatus::Error => {
                    self.erroring.insert(conn.clone());
                    self.prompting.remove(conn);
                }
                ConnectionStatus::Disconnected => {
                    self.prompting.remove(conn);
                    self.connected.remove(conn);
                    self.erroring.remove(conn);
                    self.pending_permissions.retain(|_, cid| cid != conn);
                }
            },
            AcpEvent::Error { .. } => {
                self.erroring.insert(conn.clone());
            }
            AcpEvent::PermissionRequest { request_id, .. } => {
                self.pending_permissions
                    .insert(request_id.clone(), conn.clone());
            }
            AcpEvent::TurnComplete { .. } => {
                self.prompting.remove(conn);
                // A permission request is bounded by the turn that raised it:
                // by the time TurnComplete arrives the user has either
                // approved (agent reached end_turn / refusal / max_tokens)
                // or the turn was cancelled. There is no separate event
                // when the user clicks allow/deny — the response goes
                // straight back to the agent through `responder.respond()`
                // — so this is the only deterministic place to drop the
                // entry. Without this, a single past permission would mask
                // Running across the entire app until the connection drops.
                self.pending_permissions.retain(|_, cid| cid != conn);
            }
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => match status {
                ConversationStatus::PendingReview => {
                    self.pending_reviews.insert(*conversation_id);
                }
                ConversationStatus::InProgress
                | ConversationStatus::Completed
                | ConversationStatus::Cancelled => {
                    self.pending_reviews.remove(conversation_id);
                }
            },
            _ => {}
        }
    }
}

/// Pure function: aggregate → state. Order of checks defines priority.
///
/// Priority rationale, top-down:
///
/// * `Failed` — most urgent, brief auto-recovery handles the linger.
/// * `Review` from `pending_permissions` — blocking: the agent literally
///   cannot proceed without the user clicking allow/deny, so it outranks
///   any concurrent prompt elsewhere.
/// * `Running` from `prompting` — active work outranks `pending_reviews`.
///   `pending_reviews` accumulates a `conversation_id` every time a turn
///   ends successfully and is only removed when *that specific* conversation
///   is prompted again (or the row flips to InProgress/Completed/Cancelled).
///   If `Review` were checked first, a single un-acked turn from any past
///   conversation would mask the running animation for every subsequent
///   prompt — including in unrelated conversations — until each stale row
///   was cleared individually.
/// * `Review` from `pending_reviews` — informational: surface "you have
///   un-acked turns" only when nothing is actively running.
/// * `Waiting` / `Idle` — ambient.
pub fn compute_pet_state(snapshot: &PetGlobalState) -> PetState {
    if !snapshot.erroring.is_empty() {
        return PetState::Failed;
    }
    if !snapshot.pending_permissions.is_empty() {
        return PetState::Review;
    }
    if !snapshot.prompting.is_empty() {
        return PetState::Running;
    }
    if !snapshot.pending_reviews.is_empty() {
        return PetState::Review;
    }
    if !snapshot.connected.is_empty() {
        return PetState::Waiting;
    }
    PetState::Idle
}

fn is_acp_event_relevant(payload: &serde_json::Value) -> bool {
    let Some(kind) = payload.get("type").and_then(|v| v.as_str()) else {
        return false;
    };
    matches!(
        kind,
        "status_changed"
            | "error"
            | "permission_request"
            | "turn_complete"
            | "conversation_status_changed"
    )
}

/// Map a `TurnComplete.stop_reason` to a oneshot animation, if any. Mirrors
/// the same classification `acp::lifecycle` uses to flip conversation rows
/// to `PendingReview` vs `Cancelled`, so a turn that the lifecycle treats
/// as "successful" plays a celebration here.
fn classify_turn_complete(stop_reason: &str) -> Option<PetState> {
    match stop_reason {
        "end_turn" => Some(PetState::Jumping),
        "refusal" | "max_tokens" | "max_turn_requests" | "unknown" | "empty" => {
            Some(PetState::Failed)
        }
        // `cancelled` and any future reason: stay silent.
        _ => None,
    }
}

/// Map an `app://agent-install` event payload to a oneshot animation.
/// `started` / `log` are noisy progress signals; only the terminal kinds
/// `completed` / `failed` produce a reaction.
fn classify_agent_install(payload: &serde_json::Value) -> Option<PetState> {
    let kind = payload.get("kind").and_then(|v| v.as_str())?;
    match kind {
        "completed" => Some(PetState::Jumping),
        "failed" => Some(PetState::Failed),
        _ => None,
    }
}

fn emit_oneshot(emitter: &EventEmitter, kind: PetState) {
    emit_event(emitter, "pet://oneshot", kind);
}

/// Schedule (or restart) the auto-recovery timer that will clear the
/// `erroring` set after `PET_FAILED_RECOVERY_MS`. Aborts any in-flight
/// timer first so successive errors keep the failed animation visible
/// for the full window after the *latest* error.
fn schedule_failed_recovery(
    clear_task: &mut Option<JoinHandle<()>>,
    clear_tx: &mpsc::Sender<()>,
) {
    cancel_failed_recovery(clear_task);
    let tx = clear_tx.clone();
    *clear_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(PET_FAILED_RECOVERY_MS)).await;
        // `try_send` instead of awaiting: the channel is sized for the
        // worst case (8 messages) and the main loop is the only consumer,
        // so the only way send would block is a stuck consumer — in which
        // case adding more messages can't help. A drop here just means
        // the failed animation lingers slightly longer than the window,
        // which is benign.
        let _ = tx.try_send(());
    }));
}

fn cancel_failed_recovery(clear_task: &mut Option<JoinHandle<()>>) {
    if let Some(t) = clear_task.take() {
        t.abort();
    }
}

/// Spawn-friendly subscriber loop. Mirrors `lifecycle_subscriber_task`'s
/// "subscribe synchronously, return future" shape so the broadcast buffer
/// covers the gap between `subscribe()` and the first `recv()`.
pub fn pet_state_subscriber_task(
    broadcaster: Arc<WebEventBroadcaster>,
    emitter: EventEmitter,
    handle: PetStateHandle,
) -> impl Future<Output = ()> + Send + 'static {
    let mut rx = broadcaster.subscribe();
    let (clear_tx, mut clear_rx) = mpsc::channel::<()>(8);
    async move {
        let mut snapshot = PetGlobalState::default();
        let mut last_state = PetState::Idle;
        let mut clear_task: Option<JoinHandle<()>> = None;
        // Push an initial "idle" snapshot so the renderer doesn't start blank.
        write_pet_state(&handle, last_state);
        emit_event(&emitter, "pet://state", last_state);

        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Ok(WebEvent { channel, payload }) => {
                            let payload_value = payload.as_ref();
                            let mut recompute_ambient = false;

                            match channel.as_str() {
                                "acp://event" => {
                                    if !is_acp_event_relevant(payload_value) {
                                        continue;
                                    }
                                    let envelope: EventEnvelope =
                                        match EventEnvelope::deserialize(payload_value) {
                                            Ok(env) => env,
                                            Err(err) => {
                                                eprintln!(
                                                    "[Pet] dropping malformed acp://event envelope: {err}"
                                                );
                                                continue;
                                            }
                                        };

                                    // Fire the turn_complete oneshot *before*
                                    // applying — the apply step removes the
                                    // connection from `prompting`, but the
                                    // celebration should reference the turn
                                    // that just ended either way.
                                    if let AcpEvent::TurnComplete { stop_reason, .. } =
                                        &envelope.payload
                                    {
                                        if let Some(kind) = classify_turn_complete(stop_reason) {
                                            emit_oneshot(&emitter, kind);
                                        }
                                    }

                                    let was_erroring = !snapshot.erroring.is_empty();
                                    snapshot.apply(&envelope);
                                    let now_erroring = !snapshot.erroring.is_empty();

                                    let triggered_error = matches!(
                                        envelope.payload,
                                        AcpEvent::Error { .. }
                                            | AcpEvent::StatusChanged {
                                                status: ConnectionStatus::Error,
                                            }
                                    );
                                    if triggered_error && now_erroring {
                                        schedule_failed_recovery(&mut clear_task, &clear_tx);
                                    } else if was_erroring && !now_erroring {
                                        // erroring went empty without us
                                        // firing the recovery timer — e.g.
                                        // Connected/Disconnected events that
                                        // pruned the last erroring conn —
                                        // so cancel the pending sleep to
                                        // avoid a phantom recompute later.
                                        cancel_failed_recovery(&mut clear_task);
                                    }
                                    recompute_ambient = true;
                                }
                                "folder://git-commit-succeeded"
                                | "folder://git-push-succeeded" => {
                                    emit_oneshot(&emitter, PetState::Jumping);
                                }
                                "folder://merge-aborted" => {
                                    emit_oneshot(&emitter, PetState::Failed);
                                }
                                "app://agent-install" => {
                                    if let Some(kind) = classify_agent_install(payload_value) {
                                        emit_oneshot(&emitter, kind);
                                    }
                                }
                                _ => continue,
                            }

                            if recompute_ambient {
                                let next = compute_pet_state(&snapshot);
                                if next != last_state {
                                    last_state = next;
                                    write_pet_state(&handle, next);
                                    emit_event(&emitter, "pet://state", next);
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // Broadcast buffer overrun — we can't reliably
                            // reconstruct state from the missed events, so
                            // reset to Idle and rely on the next batch of
                            // StatusChanged/Connected events to reseed the
                            // snapshot. A persistent lag without follow-up
                            // events would leave the pet stuck on idle even
                            // if connections are still active; surface it
                            // so it shows up in operator logs.
                            eprintln!(
                                "[Pet] event subscriber lagged, dropped {skipped} events; resetting to idle"
                            );
                            snapshot = PetGlobalState::default();
                            cancel_failed_recovery(&mut clear_task);
                            if last_state != PetState::Idle {
                                last_state = PetState::Idle;
                                write_pet_state(&handle, last_state);
                                emit_event(&emitter, "pet://state", last_state);
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            cancel_failed_recovery(&mut clear_task);
                            break;
                        }
                    }
                }
                Some(_) = clear_rx.recv() => {
                    // Recovery timer fired — drop the failed-state lock and
                    // recompute the ambient state from whatever else is
                    // currently active.
                    snapshot.erroring.clear();
                    clear_task = None;
                    let next = compute_pet_state(&snapshot);
                    if next != last_state {
                        last_state = next;
                        write_pet_state(&handle, next);
                        emit_event(&emitter, "pet://state", next);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, payload: AcpEvent) -> EventEnvelope {
        EventEnvelope {
            seq: 0,
            connection_id: id.to_string(),
            payload,
        }
    }

    #[test]
    fn idle_when_empty() {
        let s = PetGlobalState::default();
        assert_eq!(compute_pet_state(&s), PetState::Idle);
    }

    #[test]
    fn waiting_when_connected_but_not_prompting() {
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Waiting);
    }

    #[test]
    fn running_overrides_waiting() {
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Running);
    }

    #[test]
    fn permission_pending_yields_review() {
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::PermissionRequest {
                request_id: "r1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);
    }

    #[test]
    fn error_dominates_everything() {
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::Error {
                message: "boom".into(),
                agent_type: "claude_code".into(),
                code: None,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Failed);
    }

    #[test]
    fn running_outranks_stale_pending_review_from_other_conversation() {
        // Regression: a turn that ended in conversation 1 leaves
        // `pending_reviews = {1}`. Without the priority fix, every subsequent
        // prompt — even from a different connection / conversation — would
        // be masked as `Review` until conv 1 was acked, so the pet stops
        // showing the running animation across the whole app.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);

        s.apply(&env(
            "c2",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(
            compute_pet_state(&s),
            PetState::Running,
            "a stale pending_review must not mask an active prompt elsewhere"
        );
    }

    #[test]
    fn back_to_back_conversations_each_show_running() {
        // End-to-end of the user-reported flow: A prompts → A finishes →
        // switch to B and prompt → B finishes → back to A. Pet should show
        // Running during every prompt, regardless of un-acked reviews
        // accumulating in other conversations.
        let mut s = PetGlobalState::default();

        // A prompt.
        s.apply(&env(
            "c_a",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::InProgress,
            },
        ));
        s.apply(&env(
            "c_a",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Running);

        // A turn ends.
        s.apply(&env(
            "c_a",
            AcpEvent::TurnComplete {
                session_id: "sa".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        ));
        s.apply(&env(
            "c_a",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c_a",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);

        // Switch to B and prompt — A's review is still pending but B should
        // animate running.
        s.apply(&env(
            "c_b",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 2,
                status: ConversationStatus::InProgress,
            },
        ));
        s.apply(&env(
            "c_b",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Running);

        // B turn ends — both A and B reviews now stale.
        s.apply(&env(
            "c_b",
            AcpEvent::TurnComplete {
                session_id: "sb".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        ));
        s.apply(&env(
            "c_b",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c_b",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 2,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);

        // Back to A and prompt — B is still un-acked but A should animate.
        s.apply(&env(
            "c_a",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::InProgress,
            },
        ));
        s.apply(&env(
            "c_a",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(
            compute_pet_state(&s),
            PetState::Running,
            "returning to a prior conversation must animate even when other reviews are pending"
        );
    }

    #[test]
    fn concurrent_prompts_stay_running_through_partial_completion() {
        // Two connections prompting at the same time. When one finishes
        // (and adds itself to pending_reviews), the pet should keep showing
        // Running because the other is still actively prompting.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c2",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Running);

        // c1 finishes its turn: removed from prompting, conv 1 now pending
        // review. c2 is still prompting.
        s.apply(&env(
            "c1",
            AcpEvent::TurnComplete {
                session_id: "s1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Running);

        // c2 finishes too — now the pet is allowed to surface the reviews.
        s.apply(&env(
            "c2",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c2",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 2,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);
    }

    #[test]
    fn turn_complete_clears_permission_for_that_connection() {
        // The user responds to a permission via `RespondPermission` which
        // bypasses the broadcaster — there is no AcpEvent fired on
        // resolution. TurnComplete is the only deterministic close of the
        // permission lifecycle, so leaving entries here past TurnComplete
        // would mask Running across every conversation indefinitely.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::PermissionRequest {
                request_id: "r1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);

        s.apply(&env(
            "c1",
            AcpEvent::TurnComplete {
                session_id: "s1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));

        assert!(
            s.pending_permissions.is_empty(),
            "permission entry must not survive TurnComplete"
        );
        assert_eq!(compute_pet_state(&s), PetState::Waiting);
    }

    #[test]
    fn turn_complete_only_clears_permissions_for_finishing_connection() {
        // Concurrent turns: c1 finishes while c2 still has an outstanding
        // permission. c2's entry must survive — only the connection that
        // emitted TurnComplete is scrubbed.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c2",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::PermissionRequest {
                request_id: "r1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
        ));
        s.apply(&env(
            "c2",
            AcpEvent::PermissionRequest {
                request_id: "r2".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
        ));
        assert_eq!(s.pending_permissions.len(), 2);

        s.apply(&env(
            "c1",
            AcpEvent::TurnComplete {
                session_id: "s1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        ));

        assert_eq!(s.pending_permissions.len(), 1);
        assert!(s.pending_permissions.contains_key("r2"));
        assert_eq!(compute_pet_state(&s), PetState::Review);
    }

    #[test]
    fn permission_request_outranks_active_prompting() {
        // An outstanding permission is blocking — the agent literally can't
        // proceed without user input — so it must outrank Running even
        // when a different connection is mid-prompt.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Prompting,
            },
        ));
        s.apply(&env(
            "c2",
            AcpEvent::PermissionRequest {
                request_id: "r1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);
    }

    #[test]
    fn pending_review_visible_only_when_nothing_running() {
        // The lower-priority review check still fires when no prompts are
        // active — pet should surface that there's something un-acked once
        // the running connections settle down.
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::ConversationStatusChanged {
                conversation_id: 1,
                status: ConversationStatus::PendingReview,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Review);
    }

    #[test]
    fn disconnect_clears_state() {
        let mut s = PetGlobalState::default();
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected,
            },
        ));
        s.apply(&env(
            "c1",
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        ));
        assert_eq!(compute_pet_state(&s), PetState::Idle);
    }

    #[test]
    fn event_filter_accepts_only_pet_relevant_events() {
        for kind in [
            "status_changed",
            "error",
            "permission_request",
            "turn_complete",
            "conversation_status_changed",
        ] {
            assert!(
                is_acp_event_relevant(&serde_json::json!({ "type": kind })),
                "expected {kind} to be pet-relevant"
            );
        }

        for kind in [
            "content_delta",
            "thinking",
            "tool_call",
            "tool_call_update",
            "usage_update",
            "session_started",
        ] {
            assert!(
                !is_acp_event_relevant(&serde_json::json!({ "type": kind })),
                "expected {kind} to be ignored"
            );
        }
        assert!(!is_acp_event_relevant(&serde_json::json!({})));
    }

    #[test]
    fn classify_turn_complete_maps_known_reasons() {
        assert_eq!(classify_turn_complete("end_turn"), Some(PetState::Jumping));
        assert_eq!(classify_turn_complete("refusal"), Some(PetState::Failed));
        assert_eq!(classify_turn_complete("max_tokens"), Some(PetState::Failed));
        assert_eq!(
            classify_turn_complete("max_turn_requests"),
            Some(PetState::Failed)
        );
        assert_eq!(classify_turn_complete("unknown"), Some(PetState::Failed));
        assert_eq!(classify_turn_complete("empty"), Some(PetState::Failed));
        assert_eq!(classify_turn_complete("cancelled"), None);
        assert_eq!(classify_turn_complete("future_reason"), None);
    }

    #[test]
    fn classify_agent_install_terminal_kinds_only() {
        assert_eq!(
            classify_agent_install(&serde_json::json!({ "kind": "completed" })),
            Some(PetState::Jumping)
        );
        assert_eq!(
            classify_agent_install(&serde_json::json!({ "kind": "failed" })),
            Some(PetState::Failed)
        );
        assert_eq!(
            classify_agent_install(&serde_json::json!({ "kind": "started" })),
            None
        );
        assert_eq!(
            classify_agent_install(&serde_json::json!({ "kind": "log" })),
            None
        );
        assert_eq!(classify_agent_install(&serde_json::json!({})), None);
    }

    #[tokio::test]
    async fn subscriber_emits_oneshot_for_git_commit_succeeded() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());

        // Subscribe BEFORE spawning so we don't miss the initial idle emit.
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));

        // Drain the initial `pet://state = idle` emit.
        let _ = rx.recv().await;

        broadcaster.send(
            "folder://git-commit-succeeded",
            &serde_json::json!({ "folder_id": 1, "committed_files": 3 }),
        );

        let evt = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("oneshot should fire within 1s")
            .expect("recv");
        // Skip our own re-broadcast of the input event by reading until we see oneshot.
        let evt = if evt.channel == "folder://git-commit-succeeded" {
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("oneshot should fire within 1s")
                .expect("recv")
        } else {
            evt
        };
        assert_eq!(evt.channel, "pet://oneshot");
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("jumping"));
    }

    #[tokio::test]
    async fn subscriber_emits_oneshot_for_merge_aborted() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));
        let _ = rx.recv().await; // initial idle

        broadcaster.send(
            "folder://merge-aborted",
            &serde_json::json!({ "folder_id": 7 }),
        );

        let evt = read_until_oneshot(&mut rx).await;
        assert_eq!(evt.channel, "pet://oneshot");
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("failed"));
    }

    #[tokio::test]
    async fn subscriber_emits_oneshot_for_agent_install_completed() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));
        let _ = rx.recv().await;

        broadcaster.send(
            "app://agent-install",
            &serde_json::json!({
                "task_id": "t1",
                "kind": "completed",
                "payload": "",
            }),
        );

        let evt = read_until_oneshot(&mut rx).await;
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("jumping"));
    }

    #[tokio::test]
    async fn subscriber_emits_oneshot_for_turn_complete_end_turn() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));
        let _ = rx.recv().await;

        broadcaster.send(
            "acp://event",
            &EventEnvelope {
                seq: 1,
                connection_id: "c1".into(),
                payload: AcpEvent::TurnComplete {
                    session_id: "s".into(),
                    stop_reason: "end_turn".into(),
                    agent_type: "claude_code".into(),
                },
            },
        );

        let evt = read_until_oneshot(&mut rx).await;
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("jumping"));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_state_recovers_after_timeout() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));
        // initial idle
        let initial = rx.recv().await.unwrap();
        assert_eq!(initial.channel, "pet://state");
        assert_eq!(initial.payload.as_ref(), &serde_json::json!("idle"));

        // Drive the snapshot into Failed.
        broadcaster.send(
            "acp://event",
            &EventEnvelope {
                seq: 1,
                connection_id: "c1".into(),
                payload: AcpEvent::Error {
                    message: "boom".into(),
                    agent_type: "claude_code".into(),
                    code: None,
                },
            },
        );
        let failed = read_state_event(&mut rx).await;
        assert_eq!(failed.payload.as_ref(), &serde_json::json!("failed"));

        // Advance past the recovery window.
        tokio::time::advance(Duration::from_millis(PET_FAILED_RECOVERY_MS + 100)).await;

        let recovered = read_state_event(&mut rx).await;
        assert_eq!(recovered.payload.as_ref(), &serde_json::json!("idle"));
    }

    async fn read_until_oneshot(rx: &mut broadcast::Receiver<WebEvent>) -> WebEvent {
        loop {
            let evt = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("oneshot should fire within 1s")
                .expect("recv");
            if evt.channel == "pet://oneshot" {
                return evt;
            }
        }
    }

    async fn read_state_event(rx: &mut broadcast::Receiver<WebEvent>) -> WebEvent {
        loop {
            let evt = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("state event should fire")
                .expect("recv");
            if evt.channel == "pet://state" {
                return evt;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn second_error_extends_recovery_window() {
        // Two errors arriving 3 s apart — the second should reset the
        // recovery clock so `failed` stays visible for ~4 s after the
        // *latest* error, not 4 s from the first.
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            new_pet_state_handle(),
        ));
        let _ = rx.recv().await; // initial idle

        let send_error = |conn: &str| {
            broadcaster.send(
                "acp://event",
                &EventEnvelope {
                    seq: 1,
                    connection_id: conn.into(),
                    payload: AcpEvent::Error {
                        message: "boom".into(),
                        agent_type: "claude_code".into(),
                        code: None,
                    },
                },
            );
        };

        send_error("c1");
        let failed = read_state_event(&mut rx).await;
        assert_eq!(failed.payload.as_ref(), &serde_json::json!("failed"));

        // Halfway through the window, fire a second error. If the timer
        // were not restarted, recovery would happen at +4 s relative to
        // the first error.
        tokio::time::advance(Duration::from_millis(PET_FAILED_RECOVERY_MS / 2)).await;
        send_error("c2");

        // Advance to the *original* deadline; nothing should fire because
        // the timer was reset.
        tokio::time::advance(Duration::from_millis(PET_FAILED_RECOVERY_MS / 2 + 50)).await;
        // Drain any inbound events; we shouldn't see a state event yet.
        let mut saw_recovery = false;
        for _ in 0..5 {
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(evt)) if evt.channel == "pet://state" => {
                    if evt.payload.as_ref() == &serde_json::json!("idle") {
                        saw_recovery = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(
            !saw_recovery,
            "second error should have extended the recovery window"
        );

        // Advance past the *second* deadline.
        tokio::time::advance(Duration::from_millis(PET_FAILED_RECOVERY_MS / 2 + 100)).await;
        let recovered = read_state_event(&mut rx).await;
        assert_eq!(recovered.payload.as_ref(), &serde_json::json!("idle"));
    }

    #[tokio::test]
    async fn pet_celebrate_core_emits_oneshot() {
        use crate::commands::pet::pet_celebrate_core;
        use crate::models::pet::PetCelebrationKind;

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let mut rx = broadcaster.subscribe();

        pet_celebrate_core(&emitter, PetCelebrationKind::Jumping);

        let evt = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("oneshot should fire within 1s")
            .expect("recv");
        assert_eq!(evt.channel, "pet://oneshot");
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("jumping"));

        pet_celebrate_core(&emitter, PetCelebrationKind::Failed);
        let evt = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("oneshot should fire within 1s")
            .expect("recv");
        assert_eq!(evt.payload.as_ref(), &serde_json::json!("failed"));
    }

    #[tokio::test(start_paused = true)]
    async fn handle_resets_to_idle_on_failed_recovery_timeout() {
        // The `clear_rx` arm of the subscriber's select! also writes the
        // handle. Without it, after a brief Failed flash recovers to Idle,
        // a freshly-opened pet window would see a stale `failed` snapshot
        // for the rest of the session.
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let handle = new_pet_state_handle();
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            handle.clone(),
        ));
        let _ = rx.recv().await; // initial idle

        broadcaster.send(
            "acp://event",
            &EventEnvelope {
                seq: 1,
                connection_id: "c1".into(),
                payload: AcpEvent::Error {
                    message: "boom".into(),
                    agent_type: "claude_code".into(),
                    code: None,
                },
            },
        );
        let failed = read_state_event(&mut rx).await;
        assert_eq!(failed.payload.as_ref(), &serde_json::json!("failed"));
        assert_eq!(read_pet_state(&handle), PetState::Failed);

        tokio::time::advance(Duration::from_millis(PET_FAILED_RECOVERY_MS + 100)).await;
        let recovered = read_state_event(&mut rx).await;
        assert_eq!(recovered.payload.as_ref(), &serde_json::json!("idle"));
        assert_eq!(
            read_pet_state(&handle),
            PetState::Idle,
            "handle must follow the auto-recovery transition, not stay stuck on failed"
        );
    }

    #[tokio::test]
    async fn handle_tracks_last_emitted_ambient_state() {
        // Regression for the open-pet-mid-conversation case: the subscriber
        // de-duplicates emissions on `pet://state`, so a window opening
        // *after* the state has already settled into Running won't see any
        // event. The handle is the snapshot the freshly-mounted frontend
        // reads to fill in the gap, so it must always reflect the most
        // recent emitted ambient state.
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::WebOnly(broadcaster.clone());
        let handle = new_pet_state_handle();
        let mut rx = broadcaster.subscribe();
        tokio::spawn(pet_state_subscriber_task(
            broadcaster.clone(),
            emitter,
            handle.clone(),
        ));

        // Drain initial idle emit; handle is set in lockstep.
        let initial = read_state_event(&mut rx).await;
        assert_eq!(initial.payload.as_ref(), &serde_json::json!("idle"));
        assert_eq!(read_pet_state(&handle), PetState::Idle);

        broadcaster.send(
            "acp://event",
            &EventEnvelope {
                seq: 1,
                connection_id: "c1".into(),
                payload: AcpEvent::StatusChanged {
                    status: ConnectionStatus::Prompting,
                },
            },
        );
        let running = read_state_event(&mut rx).await;
        assert_eq!(running.payload.as_ref(), &serde_json::json!("running"));
        assert_eq!(read_pet_state(&handle), PetState::Running);

        broadcaster.send(
            "acp://event",
            &EventEnvelope {
                seq: 2,
                connection_id: "c1".into(),
                payload: AcpEvent::StatusChanged {
                    status: ConnectionStatus::Connected,
                },
            },
        );
        let waiting = read_state_event(&mut rx).await;
        assert_eq!(waiting.payload.as_ref(), &serde_json::json!("waiting"));
        assert_eq!(read_pet_state(&handle), PetState::Waiting);
    }

    #[test]
    fn pet_celebration_kind_serializes_to_snake_case() {
        use crate::models::pet::PetCelebrationKind;
        assert_eq!(
            serde_json::to_value(PetCelebrationKind::Jumping).unwrap(),
            serde_json::json!("jumping")
        );
        assert_eq!(
            serde_json::from_value::<PetCelebrationKind>(serde_json::json!("waving")).unwrap(),
            PetCelebrationKind::Waving
        );
        assert!(
            serde_json::from_value::<PetCelebrationKind>(serde_json::json!("running")).is_err(),
            "ambient state must not deserialize as a celebration kind"
        );
    }
}
