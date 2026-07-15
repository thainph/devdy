//! Approver abstraction for the broker (GĐ2 + GĐ3).
//!
//! When policy returns `Ask`, the socket handler consults an injectable
//! `Approver`. GĐ2 shipped the trait plus fail-closed / test implementations;
//! GĐ3 adds `ModalApprover`, which bridges an `Ask` to the existing permission
//! modal WITHOUT touching the broker core (that is why `start_broker` takes
//! `Arc<dyn Approver>`).
//!
//! The trait is async (GĐ3): `ModalApprover` must `await` a oneshot resolved by
//! `respond_permission`. The GĐ2 fail-closed / test impls are trivially async.

use std::sync::Arc;

use async_trait::async_trait;

use super::{ApproverResolver, BrokerRequest};
use crate::runs::permission::PermissionRequestEvent;
use crate::runs::{BrokerApprovals, BrokerRuns};
use tauri::{AppHandle, Emitter};

/// Decides whether an `Ask` request should proceed. Must be `Send + Sync` so it
/// can be shared across the accept loop's spawned tasks via `Arc`.
#[async_trait]
pub trait Approver: Send + Sync {
    /// Return `true` to allow the asked operation, `false` to deny it.
    async fn approve(&self, req: &BrokerRequest, reason: &str) -> bool;
}

/// GĐ2 default: deny every `Ask` (fail-closed). Used when no modal is wired.
#[allow(dead_code)] // default approver, selected once the broker is wired (GĐ3)
pub struct DenyAllApprover;

#[async_trait]
impl Approver for DenyAllApprover {
    async fn approve(&self, _req: &BrokerRequest, _reason: &str) -> bool {
        false
    }
}

/// Test-only fixed-decision approver, used to exercise both branches of the
/// socket round-trip.
#[allow(dead_code)]
pub struct FixedApprover(pub bool);

#[async_trait]
impl Approver for FixedApprover {
    async fn approve(&self, _req: &BrokerRequest, _reason: &str) -> bool {
        self.0
    }
}

/// GĐ3 modal approver: on `Ask`, emit the existing `PermissionRequestEvent` to
/// the frontend and await the user's decision. The decision arrives back through
/// the unchanged `respond_permission` command, which resolves the oneshot we
/// register in `BrokerApprovals` (keyed by a fresh `request_id`).
///
/// Reuses the modal end-to-end: same event topic, same payload shape, same
/// frontend handler. No new command / modal is introduced.
pub struct ModalApprover {
    app: AppHandle,
    run_id: String,
    approvals: BrokerApprovals,
    /// Project path, surfaced as the request's `cwd` for the modal.
    cwd: Option<String>,
}

impl ModalApprover {
    pub fn new(
        app: AppHandle,
        run_id: String,
        approvals: BrokerApprovals,
        cwd: Option<String>,
    ) -> Self {
        Self { app, run_id, approvals, cwd }
    }
}

#[async_trait]
impl Approver for ModalApprover {
    async fn approve(&self, req: &BrokerRequest, reason: &str) -> bool {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();

        // 1. Register the pending approval so respond_permission can resolve it.
        {
            let mut pending = self.approvals.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        // 2. Emit the SAME event the sidecar permission flow uses. The frontend
        //    cannot tell the source apart; the modal renders identically.
        let cmd_display = format!("{} {}", req.tool, req.argv.join(" "));
        let evt = PermissionRequestEvent {
            run_id: self.run_id.clone(),
            request_id: request_id.clone(),
            tool_name: req.tool.clone(),
            tool_input: serde_json::json!({ "command": cmd_display.trim() }),
            session_id: None,
            cwd: self.cwd.clone(),
            title: Some(format!("Cho phép `{}`?", cmd_display.trim())),
            description: Some(reason.to_string()),
            display_name: Some(format!("{} command", req.tool)),
        };
        let topic = format!("run:permission_request:{}", self.run_id);
        if self.app.emit(&topic, evt).is_err() {
            // Emit failed → clean up and fail-closed deny.
            self.approvals.lock().await.remove(&request_id);
            return false;
        }

        // 3. Await the user's decision. A dropped sender (run ended) → deny.
        match rx.await {
            Ok(allow) => allow,
            Err(_) => false,
        }
    }
}

/// GĐ7 resolver for the app-wide singleton broker. Given a request's `run_id`,
/// it looks the run up in `BrokerRuns`; if the run is alive it builds a
/// `ModalApprover` bound to that run (with its cwd for the modal), otherwise it
/// returns `None` so the broker fail-closed denies the `Ask`.
///
/// Reuses the exact same modal machinery as before — only the *lifetime* of the
/// broker changed (one socket for the whole app, not one per run).
pub struct ModalApproverResolver {
    app: AppHandle,
    approvals: BrokerApprovals,
    runs: BrokerRuns,
}

impl ModalApproverResolver {
    pub fn new(app: AppHandle, approvals: BrokerApprovals, runs: BrokerRuns) -> Self {
        Self { app, approvals, runs }
    }
}

#[async_trait]
impl ApproverResolver for ModalApproverResolver {
    async fn resolve(&self, run_id: Option<&str>) -> Option<Arc<dyn Approver>> {
        let run_id = run_id?;
        // Copy the run's context out under a short synchronous lock; None (run
        // not registered / already ended) → caller fail-closed denies.
        let ctx = self.runs.lock().ok()?.get(run_id)?.clone();
        Some(Arc::new(ModalApprover::new(
            self.app.clone(),
            run_id.to_string(),
            self.approvals.clone(),
            ctx.cwd,
        )))
    }
}
