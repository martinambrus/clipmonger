//! One bounded compact-history request at a time.
use crate::ipc_bridge::{CompactRequest, UiCmd};
use crate::popup::CompactRow;
use clipmonger_ipc::proto::methods::{
    HistoryCompactCancelParams, HistoryCompactParams, HistoryCompactResult,
};

pub(super) struct Compact {
    owner: Option<u64>,
    generation: u64,
    request: u64,
    restore_epoch: u64,
    page: u64,
    query: String,
    cursor: Option<String>,
    pending: Option<CompactRequest>,
    wanted: bool,
    pub loading: bool,
    pub done: bool,
    pub error: bool,
    pub error_code: Option<String>,
}
impl Compact {
    pub fn new() -> Self {
        Self {
            owner: clipmonger_core::entropy::random_u64().ok(),
            generation: 0,
            request: 0,
            restore_epoch: 0,
            page: 0,
            query: String::new(),
            cursor: None,
            pending: None,
            wanted: false,
            loading: false,
            done: false,
            error: false,
            error_code: None,
        }
    }
    fn cancel(&self, cursor: String, query: String, generation: u64) -> Option<UiCmd> {
        Some(UiCmd::CancelCompactHistory(HistoryCompactCancelParams {
            owner: self.owner?,
            generation,
            query,
            cursor,
        }))
    }
    pub fn reset(&mut self, query: String, active: bool, restore: bool) -> Option<UiCmd> {
        let cancel = self
            .cursor
            .take()
            .and_then(|c| self.cancel(c, self.query.clone(), self.generation));
        let next = self.generation.checked_add(1);
        let restore_next = if restore {
            self.restore_epoch.checked_add(1)
        } else {
            Some(self.restore_epoch)
        };
        if let Some(epoch) = restore_next {
            self.restore_epoch = epoch;
        }
        self.query = query;
        self.page = 0;
        self.done = false;
        self.error = next.is_none() || restore_next.is_none() || self.owner.is_none();
        self.error_code = None;
        self.wanted = active && !self.error;
        self.loading = self.wanted;
        if let Some(next) = next {
            self.generation = next;
        }
        cancel
    }
    pub fn query_matches(&self, query: &str) -> bool {
        self.query == query
    }
    pub fn matches(&self, tag: &CompactRequest) -> bool {
        tag.params.generation == self.generation
            && tag.restore_epoch == self.restore_epoch
            && tag.page == self.page
            && tag.params.query == self.query
    }
    pub fn accepts(&self, tag: &CompactRequest) -> bool {
        self.pending.as_ref() == Some(tag) && self.matches(tag)
    }
    pub fn next_page(&mut self) {
        if !self.loading && !self.done && self.cursor.is_some() {
            if let Some(page) = self.page.checked_add(1) {
                self.page = page;
                self.wanted = true;
                self.loading = true;
            } else {
                self.error = true;
            }
        }
    }
    pub fn command(&mut self) -> Option<UiCmd> {
        if !self.wanted || self.pending.is_some() {
            return None;
        }
        let Some(request) = self.request.checked_add(1) else {
            self.error = true;
            self.loading = false;
            self.wanted = false;
            return None;
        };
        self.request = request;
        let tag = CompactRequest {
            params: HistoryCompactParams {
                owner: self.owner?,
                generation: self.generation,
                query: self.query.clone(),
                cursor: self.cursor.clone(),
                byte_budget: 65536,
                candidate_budget: 32,
                row_budget: 50,
            },
            page: self.page,
            request,
            restore_epoch: self.restore_epoch,
        };
        self.pending = Some(tag.clone());
        self.wanted = false;
        Some(UiCmd::CompactHistory(tag))
    }
    pub fn result(
        &mut self,
        tag: CompactRequest,
        result: Result<serde_json::Value, clipmonger_ipc::proto::IpcError>,
    ) -> (Option<Vec<CompactRow>>, Option<UiCmd>) {
        if self.pending.as_ref() != Some(&tag) {
            let stale =
                tag.params.generation != self.generation || tag.restore_epoch != self.restore_epoch;
            let cancel = if stale {
                result
                    .ok()
                    .and_then(|body| serde_json::from_value::<HistoryCompactResult>(body).ok())
                    .and_then(|body| body.cursor)
                    .and_then(|cursor| self.cancel(cursor, tag.params.query, tag.params.generation))
            } else {
                None
            };
            return (None, cancel);
        }
        self.pending = None;
        let stale = tag.params.generation != self.generation
            || tag.restore_epoch != self.restore_epoch
            || tag.page != self.page
            || tag.params.query != self.query;
        if stale {
            let cancel = result
                .ok()
                .and_then(|body| serde_json::from_value::<HistoryCompactResult>(body).ok())
                .and_then(|body| body.cursor)
                .and_then(|cursor| self.cancel(cursor, tag.params.query, tag.params.generation));
            return (None, cancel);
        }
        if let Err(error) = &result {
            self.error_code = Some(error.code.clone());
        }
        let returned_cursor = result
            .as_ref()
            .ok()
            .and_then(|body| body.get("cursor"))
            .and_then(serde_json::Value::as_str)
            .filter(|cursor| cursor.len() <= 128)
            .map(str::to_owned);
        let parsed = result
            .ok()
            .and_then(|body| serde_json::from_value::<HistoryCompactResult>(body).ok())
            .filter(|body| {
                body.owner == tag.params.owner
                    && body.generation == tag.params.generation
                    && body.rows.len() <= 50
                    && body.done == body.cursor.is_none()
            });
        let Some(parsed) = parsed else {
            self.error = true;
            self.loading = false;
            let cancel =
                self.cursor.take().or(returned_cursor).and_then(|cursor| {
                    self.cancel(cursor, tag.params.query, tag.params.generation)
                });
            return (None, cancel);
        };
        let rows: Result<Vec<_>, _> = parsed.rows.into_iter().map(CompactRow::try_from).collect();
        let Ok(rows) = rows else {
            self.error = true;
            self.loading = false;
            return (
                None,
                parsed.cursor.and_then(|cursor| {
                    self.cancel(cursor, tag.params.query, tag.params.generation)
                }),
            );
        };
        self.cursor = parsed.cursor;
        self.done = parsed.done;
        self.loading = rows.is_empty() && !self.done;
        self.wanted = self.loading;
        if self.loading {
            (None, None)
        } else {
            (Some(rows), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn command(state: &mut Compact) -> CompactRequest {
        match state.command().expect("request") {
            UiCmd::CompactHistory(tag) => tag,
            _ => panic!("wrong command"),
        }
    }
    fn reply(
        tag: &CompactRequest,
        cursor: Option<&str>,
        rows: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        json!({"owner": tag.params.owner, "generation":tag.params.generation, "rows":rows, "cursor":cursor, "done":cursor.is_none(), "bytes_read":0, "candidates":0})
    }
    fn row() -> serde_json::Value {
        json!({"id":"00000000-0000-0000-0000-000000000001","pinned":false,"last_used_ms":1,"preview":"short","preview_truncated":true,"kind":"image","thumbnail_hash":"af63dc4c8601ec8c"})
    }
    #[test]
    fn obsolete_begin_is_cancelled_and_latest_query_is_coalesced() {
        let mut state = Compact::new();
        state.reset("old".into(), true, false);
        let old = command(&mut state);
        state.reset("intermediate".into(), true, false);
        state.reset("new".into(), true, false);
        assert!(state.command().is_none());
        let (rows, cancel) =
            state.result(old.clone(), Ok(reply(&old, Some("old-token"), vec![row()])));
        assert!(rows.is_none());
        assert!(
            matches!(cancel, Some(UiCmd::CancelCompactHistory(p)) if p.cursor == "old-token" && p.query == "old")
        );
        let new = command(&mut state);
        assert_eq!(new.params.query, "new");
        let (rows, _) = state.result(new.clone(), Ok(reply(&new, None, vec![row()])));
        assert_eq!(rows.unwrap()[0].preview, "short");
        assert!(state.done);
    }
    #[test]
    fn sparse_progress_pages_and_restore_drop_old_errors() {
        let mut state = Compact::new();
        state.reset("needle".into(), true, false);
        let first = command(&mut state);
        assert!(state
            .result(first.clone(), Ok(reply(&first, Some("cursor"), vec![])))
            .0
            .is_none());
        assert!(state.loading);
        let progress = command(&mut state);
        assert_eq!(progress.params.cursor.as_deref(), Some("cursor"));
        let (rows, _) = state.result(
            progress.clone(),
            Ok(reply(&progress, Some("cursor"), vec![row()])),
        );
        assert_eq!(rows.unwrap().len(), 1);
        state.next_page();
        let page = command(&mut state);
        assert_eq!(page.page, 1);
        assert!(matches!(
            state.reset("needle".into(), true, true),
            Some(UiCmd::CancelCompactHistory(_))
        ));
        let error = clipmonger_ipc::proto::IpcError::new("unavailable", "late".into());
        assert!(state.result(page, Err(error)).0.is_none());
        assert!(!state.error);
        let fresh = command(&mut state);
        assert_eq!(fresh.page, 0);
        assert_eq!(fresh.params.cursor, None);
        assert_ne!(fresh.restore_epoch, progress.restore_epoch);
    }
}

#[cfg(test)]
mod app_bridge_test {
    use crate::app::{AppParts, PopupApp};
    use crate::ipc_bridge::{Bridge, Transport};
    use crate::runner::{App, WindowKind};
    use clipmonger_ipc::proto::IpcError;
    use serde_json::{json, Value};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    struct Fixture;
    impl Transport for Fixture {
        fn call(&mut self, method: &str, params: &Value) -> Result<Value, IpcError> {
            Ok(match method {
                "history.compact" => {
                    json!({"owner":params["owner"],"generation":params["generation"],"rows":[{"id":"00000000-0000-0000-0000-000000000001","pinned":false,"last_used_ms":1,"preview":"compact hit","preview_truncated":false,"kind":"text","thumbnail_hash":null}],"cursor":null,"done":true,"bytes_read":1,"candidates":1})
                }
                "history.list" => {
                    assert_eq!(params["limit"], 1);
                    json!({"clips":[{"id":"00000000-0000-0000-0000-000000000001","created_ms":1,"source_os":"test","preview":"compact hit","formats":["text/plain"],"content_hash":"af63dc4c8601ec8c","rev":{"wall_ms":1,"dev":"test"}}]})
                }
                _ => json!({}),
            })
        }
    }
    #[test]
    fn popup_consumes_compact_reply_from_real_bridge() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(Fixture), wake);
        let mut app = PopupApp::new(AppParts {
            profile: "fixture".into(),
            cmds,
            events,
            tray: Box::new(Vec::new),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        let ctx = egui::Context::default();
        let end = Instant::now() + Duration::from_secs(3);
        while app.vm().current().is_none() {
            wake_rx
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .expect("bridge reply");
            ctx.run_logic(&egui::RawInput::default(), |ctx| {
                app.logic(ctx, WindowKind::Popup)
            });
        }
        assert_eq!(app.vm().compact_rows().unwrap()[0].preview, "compact hit");
        assert_eq!(app.vm().current().unwrap().id, uuid::Uuid::from_u128(1));
    }
}
