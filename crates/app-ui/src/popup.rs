//! Popup view-model: clips, query, selection and banner state.

use clipmonger_core::model::Clip;
use clipmonger_core::store::{CompactHit, CompactKind};
use serde_json::Value;
use uuid::Uuid;

use crate::{keymap, search, strings, target_session};

/// Effect the shell or bridge must execute for one key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmEffect {
    /// Nothing to do.
    None,
    /// Paste this clip through the validated target.
    Paste(Uuid),
    /// Close the popup without acting.
    Dismiss,
    /// Ask the daemon to delete this clip.
    Remove(Uuid),
    /// Ask the daemon to pin this clip.
    Pin(Uuid),
    /// Re-fetch the list.
    Refresh,
    /// Switch to permanent group tab `1..=9`.
    GroupTab(u8),
    /// Execute this permanent clip.
    ExecPermanent(Uuid),
}

/// Preview-kind string id for one clip, from its MIME formats.
#[must_use]
pub fn preview_kind(clip: &Clip) -> &'static str {
    if clip.formats.iter().any(|f| f.starts_with("image/")) {
        "popup.kind_image"
    } else if clip.formats.iter().any(|f| f.contains("html")) {
        "popup.kind_html"
    } else if clip.formats.iter().any(|f| f.contains("uri-list")) {
        "popup.kind_file"
    } else if clip.formats.is_empty()
        || clip.formats.iter().any(|f| {
            f.split(';')
                .next()
                .map(str::trim)
                .is_some_and(|b| b.starts_with("text/"))
        })
    {
        "popup.kind_text"
    } else {
        "popup.kind_other"
    }
}

/// Display-only truncation for history previews.
///
/// Keeps at most `lines` lines (`popup.preview_lines`; `\n`, `\r\n` and `\r`
/// break lines, a trailing break is dropped) and at most `width` Unicode scalars
/// in total (`popup.preview_width`; line breaks do not count). The total
/// visible budget includes the deterministic ellipsis `…`, appended whenever
/// characters or lines were cut. Zero `lines` is treated as 1. Width 1 is valid and
/// yields `…` when truncation is needed. Invalid zero (reachable via manual
/// TOML edit or `CLIPMONGER_POPUP_PREVIEW_WIDTH=0` env override despite
/// validation requiring ≥1) is clamped to 1 for safe display without panic.
/// Huge widths do not preallocate huge capacity: short previews are returned
/// verbatim without allocating `width` bytes, and truncation only collects the
/// needed `width-1` scalars. Never slices UTF-8 by byte offsets.
#[must_use]
pub fn display_preview(preview: &str, width: u32, lines: u32) -> String {
    display_preview_cut(preview, width, lines).0
}

/// [`display_preview`] and whether it cut characters or lines (the ellipsis
/// was appended).
#[must_use]
pub fn display_preview_cut(preview: &str, width: u32, lines: u32) -> (String, bool) {
    let budget = width.max(1) as usize;
    let max_lines = lines.max(1);
    let mut out = String::new();
    let mut used = 0;
    let mut line = 1;
    let mut cut = false;
    let mut chars = preview.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if c == '\n' || c == '\r' {
            if chars.peek().is_none() {
                break;
            }
            if line == max_lines {
                cut = true;
                break;
            }
            line += 1;
            out.push('\n');
            continue;
        }
        if used == budget {
            cut = true;
            break;
        }
        out.push(c);
        used += 1;
    }
    if cut {
        if used == budget {
            // Make room for the ellipsis inside the character budget.
            while out.ends_with('\n') {
                out.pop();
            }
            out.pop();
        }
        out.push('…');
    }
    (out, cut)
}

/// Whether a special key reaches the view-model while the search field is
/// being edited: navigation and commit keys forward, text-editing keys stay
/// with the field (Delete there edits text, never soft-deletes a clip).
#[must_use]
pub fn forwards_key_when_editing(name: &str) -> bool {
    matches!(
        name,
        "ArrowUp"
            | "ArrowDown"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | "Enter"
            | "Escape"
            | "F5"
    )
}

/// What applying a history-list reply did to the popup state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryApply {
    /// The reply holds a different list, which replaced the shown one.
    Changed,
    /// The reply holds the list that is shown already (an empty one included).
    Unchanged,
    /// The reply is not a history list; the state is untouched.
    Malformed,
}

/// The clips of a history-list reply body, `None` when the body is malformed.
#[must_use]
pub fn parse_history(body: &Value) -> Option<Vec<Clip>> {
    let clips = body.get("clips")?;
    serde_json::from_value::<Vec<Clip>>(clips.clone()).ok()
}

/// A history row projected by the daemon. Unlike `Clip`, it contains no
/// payload or search text; the daemon has already evaluated the full query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactRow {
    pub id: Uuid,
    pub pinned: bool,
    pub preview: String,
    pub kind: CompactKind,
    pub thumbnail_hash: Option<String>,
    /// Permanent group of a search result that is a permanent clip.
    pub group: Option<String>,
}

impl CompactRow {
    /// A search-result row for a permanent clip, shown by its name.
    #[must_use]
    pub fn from_permanent(clip: &clipmonger_core::model::PermanentClip) -> Self {
        Self {
            id: clip.id,
            pinned: false,
            preview: clip.name.clone(),
            kind: CompactKind::Text,
            thumbnail_hash: None,
            group: Some(clip.group.clone()),
        }
    }
}

impl TryFrom<CompactHit> for CompactRow {
    type Error = ();

    fn try_from(hit: CompactHit) -> Result<Self, Self::Error> {
        let id = Uuid::parse_str(&hit.id).map_err(|_| ())?;
        if hit.thumbnail_hash.as_ref().is_some_and(|hash| {
            hit.kind != CompactKind::Image || !clipmonger_core::model::valid_clip_content_hash(hash)
        }) {
            return Err(());
        }
        Ok(Self {
            id,
            pinned: hit.pinned,
            preview: hit.preview,
            kind: hit.kind,
            thumbnail_hash: hit.thumbnail_hash,
            group: None,
        })
    }
}

/// Number of rows a page key moves the selection.
const PAGE_ROWS: isize = 10;

/// Renderer-free popup state; the shell renders it verbatim.
#[derive(Debug)]
pub struct PopupVm {
    /// Current search text, edited through [`Self::on_key`].
    pub query: String,
    /// Selected row within [`Self::visible`].
    pub selected: usize,
    clips: Vec<Clip>,
    compact_rows: Option<Vec<CompactRow>>,
    /// Matches of a finished search across all its pages.
    result_total: Option<usize>,
    current: Option<Clip>,
    offline: bool,
    target_valid: bool,
    paused: bool,
    /// Banner text for a popup chord another application holds.
    hotkey_conflict: String,
    keymap: keymap::Keymap,
    /// Target session wired to the paste guard.
    pub target_session: target_session::TargetSession,
}

impl Default for PopupVm {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupVm {
    /// Build an empty, online popup with valid target and defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            clips: Vec::new(),
            compact_rows: None,
            result_total: None,
            current: Option::None,
            offline: false,
            target_valid: true,
            paused: false,
            hotkey_conflict: String::new(),
            keymap: keymap::Keymap::defaults(),
            target_session: target_session::TargetSession::new(),
        }
    }

    /// Replace the clip list and reset the selection.
    pub fn set_clips(&mut self, clips: Vec<Clip>) {
        self.compact_rows = None;
        self.clips = clips;
        self.selected = 0;
        self.clamp_selection();
    }

    /// Replace the displayed compact page. Its rows are already matched by
    /// the server, including matches outside their display previews.
    pub fn set_compact_rows(&mut self, rows: Vec<CompactRow>) {
        self.clips.clear();
        self.compact_rows = Some(rows);
        self.selected = 0;
    }

    /// Set the number of search matches shown on the status line.
    pub fn set_result_total(&mut self, total: Option<usize>) {
        self.result_total = total;
    }

    /// The current compact page, if compact history is active.
    #[must_use]
    pub fn compact_rows(&self) -> Option<&[CompactRow]> {
        self.compact_rows.as_deref()
    }

    fn selected_id(&self) -> Option<Uuid> {
        if let Some(rows) = &self.compact_rows {
            rows.get(self.selected).map(|row| row.id)
        } else {
            self.clip_at(self.selected).map(|clip| clip.id)
        }
    }

    /// Id of the selected row when it is a permanent clip in search results.
    fn selected_permanent(&self) -> Option<Uuid> {
        self.compact_rows
            .as_ref()?
            .get(self.selected)
            .filter(|row| row.group.is_some())
            .map(|row| row.id)
    }

    /// Set the current-clipboard slot shown above the list.
    pub fn set_current(&mut self, current: Option<Clip>) {
        self.current = current;
    }

    /// Read the current-clipboard slot.
    #[must_use]
    pub fn current(&self) -> Option<&Clip> {
        self.current.as_ref()
    }

    /// Mark the daemon link down or up.
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Record the popup chord another application holds (`None` clears it),
    /// from the daemon's `caps.popup_hotkey_conflict`.
    pub fn set_hotkey_conflict(&mut self, chord: Option<&str>) {
        self.hotkey_conflict = chord.map_or_else(String::new, |chord| {
            strings::text("popup.hotkey_conflict").replace("{chord}", chord)
        });
    }

    /// Mark the captured target stale or valid.
    pub fn set_target_valid(&mut self, valid: bool) {
        self.target_valid = valid;
        if valid {
            self.target_session
                .capture_target("current-target".to_string());
        } else {
            self.target_session.focus_lost();
        }
    }

    /// Stored clips in list order.
    #[must_use]
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// Apply popup.keys overrides live to the keymap.
    ///
    /// # Errors
    /// Returns the parse/binding error text for a bad override.
    pub fn apply_keys(
        &mut self,
        keys: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        self.keymap = keymap::Keymap::from_config_keys(keys)?;
        Ok(())
    }

    /// Test exact native shortcut ownership before egui's clipboard translation.
    pub(crate) fn has_binding(&self, event: &keymap::KeyEvent) -> bool {
        self.keymap.has_binding(event)
    }

    /// Resolve a key event to its action via the current keymap.
    #[must_use]
    pub fn action_for(&self, event: &keymap::KeyEvent) -> Option<keymap::Action> {
        self.keymap.action_for(event)
    }

    /// Mark monitoring paused or running for the status line.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Drop one clip after the server confirms the delete.
    pub fn remove_clip(&mut self, id: Uuid) {
        self.clips.retain(|clip| clip.id != id);
        if let Some(rows) = &mut self.compact_rows {
            rows.retain(|row| row.id != id);
        }
        self.clamp_selection();
    }

    /// Apply the server-confirmed pin flag to one clip.
    pub fn apply_pin(&mut self, id: Uuid, pinned: bool) {
        if let Some(row) = self
            .compact_rows
            .as_mut()
            .and_then(|rows| rows.iter_mut().find(|row| row.id == id))
        {
            row.pinned = pinned;
        }
        if let Some(clip) = self.clips.iter_mut().find(|clip| clip.id == id) {
            clip.pinned = pinned;
        }
    }

    /// Read the stored pin flag for one clip.
    #[must_use]
    pub fn is_pinned(&self, id: Uuid) -> bool {
        self.clips.iter().any(|clip| clip.id == id && clip.pinned)
            || self
                .compact_rows
                .as_ref()
                .is_some_and(|rows| rows.iter().any(|row| row.id == id && row.pinned))
    }

    /// Replace the list from a history-list reply body.
    pub fn apply_history(&mut self, body: &Value) -> HistoryApply {
        let Some(list) = parse_history(body) else {
            return HistoryApply::Malformed;
        };
        self.compact_rows = None;
        if self.clips.len() == list.len()
            && self.clips.iter().zip(list.iter()).all(|(a, b)| {
                a.id == b.id
                    && a.preview == b.preview
                    && a.pinned == b.pinned
                    && a.content_hash == b.content_hash
                    && a.formats == b.formats
            })
        {
            return HistoryApply::Unchanged;
        }
        let prev_len = self.clips.len();
        self.clips = list;
        if self.clips.len() != prev_len {
            self.clamp_selection();
        }
        HistoryApply::Changed
    }

    /// Clip at a visible row, if any.
    #[must_use]
    pub fn clip_at(&self, row: usize) -> Option<&Clip> {
        self.visible()
            .get(row)
            .and_then(|index| self.clips.get(*index))
    }

    /// Search-filtered clip indices in list order.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        self.compact_rows.as_ref().map_or_else(
            || search::filter_clips(&self.clips, &self.query),
            |rows| (0..rows.len()).collect(),
        )
    }

    /// Whether the banner is an alert drawn in the error colour: a popup
    /// chord another application holds.
    #[must_use]
    pub fn banner_alert(&self) -> bool {
        !self.offline && !self.hotkey_conflict.is_empty()
    }

    /// Banner text for the current state, empty when all is well.
    #[must_use]
    pub fn banner(&self) -> &str {
        if self.offline {
            strings::text("popup.offline")
        } else if !self.hotkey_conflict.is_empty() {
            &self.hotkey_conflict
        } else if !self.target_valid {
            strings::text("popup.stale_target")
        } else {
            ""
        }
    }

    /// Status line: paused note or item count.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.paused {
            strings::text("popup.paused").to_string()
        } else if let Some(total) = self.result_total {
            strings::text("popup.results").replace("{count}", &total.to_string())
        } else {
            strings::text("popup.count").replace(
                "{count}",
                &self
                    .compact_rows
                    .as_ref()
                    .map_or(self.clips.len(), Vec::len)
                    .to_string(),
            )
        }
    }

    /// Clamp the selection into the visible rows.
    pub(crate) fn clamp_selection(&mut self) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Move the selection by a signed row delta.
    fn move_selection(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let last = len.saturating_sub(1);
        self.selected = self
            .selected
            .min(last)
            .saturating_add_signed(delta)
            .min(last);
    }

    /// Handle one key press and report the resulting effect.
    #[allow(clippy::needless_pass_by_value)]
    pub fn on_key(&mut self, event: keymap::KeyEvent) -> VmEffect {
        if event.key == "Backspace" && !event.ctrl && !event.alt && !event.super_key {
            self.query.pop();
            self.clamp_selection();
            return VmEffect::None;
        }
        let Some(action) = self.keymap.action_for(&event) else {
            return VmEffect::None;
        };
        self.on_action(action)
    }

    /// Apply an already resolved action to the history list.
    pub fn on_action(&mut self, action: keymap::Action) -> VmEffect {
        match action {
            keymap::Action::NavigateUp => {
                self.move_selection(-1);
                VmEffect::None
            }
            keymap::Action::NavigateDown => {
                self.move_selection(1);
                VmEffect::None
            }
            keymap::Action::PageUp => {
                self.move_selection(-PAGE_ROWS);
                VmEffect::None
            }
            keymap::Action::PageDown => {
                self.move_selection(PAGE_ROWS);
                VmEffect::None
            }
            keymap::Action::Home => {
                self.selected = 0;
                VmEffect::None
            }
            keymap::Action::End => {
                let len = self.visible().len();
                self.selected = len.saturating_sub(1);
                VmEffect::None
            }
            keymap::Action::TypeChar(c) => {
                self.query.push(c);
                self.clamp_selection();
                VmEffect::None
            }
            keymap::Action::Paste | keymap::Action::PasteInto => {
                if let Some(id) = self.selected_permanent() {
                    return VmEffect::ExecPermanent(id);
                }
                if !self.target_valid {
                    return VmEffect::None;
                }
                self.selected_id().map_or(VmEffect::None, VmEffect::Paste)
            }
            keymap::Action::Dismiss => VmEffect::Dismiss,
            keymap::Action::Remove if self.selected_permanent().is_none() => {
                self.selected_id().map_or(VmEffect::None, VmEffect::Remove)
            }
            keymap::Action::Pin if self.selected_permanent().is_none() => {
                self.selected_id().map_or(VmEffect::None, VmEffect::Pin)
            }
            keymap::Action::Refresh => VmEffect::Refresh,
            keymap::Action::GroupTab(n) => VmEffect::GroupTab(n),
            keymap::Action::Remove
            | keymap::Action::Pin
            | keymap::Action::Edit
            | keymap::Action::None => VmEffect::None,
        }
    }
}

#[cfg(test)]
mod compact_row_tests {
    use super::*;
    #[test]
    fn server_matches_are_not_filtered_by_preview_and_actions_use_row_id() {
        let id = Uuid::from_u128(42);
        let mut vm = PopupVm::new();
        vm.query = "match beyond preview".into();
        vm.set_compact_rows(vec![CompactRow {
            id,
            pinned: false,
            preview: "short".into(),
            kind: CompactKind::Text,
            thumbnail_hash: None,
            group: None,
        }]);
        assert_eq!(vm.visible(), vec![0]);
        assert_eq!(vm.on_action(keymap::Action::Paste), VmEffect::Paste(id));
        assert_eq!(vm.on_action(keymap::Action::Pin), VmEffect::Pin(id));
        vm.apply_pin(id, true);
        assert!(vm.is_pinned(id));
        vm.remove_clip(id);
        assert_eq!(vm.visible(), [] as [usize; 0]);
    }

    #[test]
    fn permanent_results_execute_and_cannot_be_pinned_or_deleted() {
        let id = Uuid::from_u128(7);
        let mut vm = PopupVm::new();
        vm.query = "logo".into();
        vm.set_compact_rows(vec![CompactRow {
            id,
            pinned: false,
            preview: "Company logo".into(),
            kind: CompactKind::Text,
            thumbnail_hash: None,
            group: Some("Brand".into()),
        }]);
        assert_eq!(
            vm.on_action(keymap::Action::Paste),
            VmEffect::ExecPermanent(id)
        );
        assert_eq!(vm.on_action(keymap::Action::Pin), VmEffect::None);
        assert_eq!(vm.on_action(keymap::Action::Remove), VmEffect::None);
    }
}

#[cfg(test)]
mod display_preview_tests {
    use super::display_preview;

    #[test]
    fn empty_and_short_preserved() {
        assert_eq!(display_preview("", 5, 1), "");
        assert_eq!(display_preview("hi", 80, 1), "hi");
        assert_eq!(display_preview("abc", 3, 1), "abc");
    }

    #[test]
    fn width_one_ellipsis() {
        assert_eq!(display_preview("abcdef", 1, 1), "…");
        assert_eq!(display_preview("a", 1, 1), "a");
        assert_eq!(display_preview("", 1, 1), "");
    }

    #[test]
    fn truncates_with_ellipsis_budget() {
        assert_eq!(display_preview("abcdefghij", 5, 1), "abcd…");
        assert_eq!(display_preview("abcdefghij", 5, 1).chars().count(), 5);
    }

    #[test]
    fn multibyte_scalar() {
        let preview = "🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀";
        assert_eq!(display_preview(preview, 5, 1), "🦀🦀🦀🦀…");
        assert_eq!(display_preview(preview, 5, 1).chars().count(), 5);
    }

    #[test]
    fn zero_safe() {
        // invalid zero treated as 1
        assert_eq!(display_preview("abcdef", 0, 1), "…");
        assert_eq!(display_preview("", 0, 1), "");
        assert_eq!(display_preview("a", 0, 1), "a");
    }

    #[test]
    fn huge_width_no_huge_alloc() {
        let short = "abc";
        // huge width should return verbatim without allocating huge capacity
        assert_eq!(display_preview(short, 1_000_000, 1), "abc");
        let long = "x".repeat(200);
        assert_eq!(display_preview(&long, 1_000_000, 1), long);
    }

    #[test]
    fn multiline_keeps_lines_and_counts_only_chars() {
        assert_eq!(display_preview("ab\ncd", 80, 3), "ab\ncd");
        assert_eq!(display_preview("ab\ncd\nef\ngh", 80, 3), "ab\ncd\nef…");
        assert_eq!(display_preview("ab\ncd\nef", 80, 1), "ab…");
        // Line breaks do not count toward the width.
        assert_eq!(display_preview("ab\ncd", 4, 3), "ab\ncd");
        assert_eq!(display_preview("ab\ncdef", 4, 3), "ab\nc…");
        assert_eq!(display_preview("ab\ncd", 1, 3), "…");
    }

    #[test]
    fn crlf_and_trailing_break() {
        assert_eq!(display_preview("ab\r\ncd\r\n", 80, 3), "ab\ncd");
        assert_eq!(display_preview("ab\rcd", 80, 3), "ab\ncd");
        assert_eq!(display_preview("ab\n", 80, 1), "ab");
        assert_eq!(display_preview("ab\r\n\r\ncd", 80, 1), "ab…");
    }

    #[test]
    fn zero_lines_is_one() {
        assert_eq!(display_preview("ab\ncd", 80, 0), "ab…");
    }

    #[test]
    fn does_not_slice_bytes() {
        let s = "a🦀b";
        // ensure truncation respects char boundaries, not bytes
        let out = display_preview(s, 2, 1);
        assert_eq!(out, "a…");
        assert_eq!(out.chars().count(), 2);
    }

    #[test]
    fn cut_flag_follows_the_ellipsis() {
        use super::display_preview_cut;
        // Normalized but complete previews are not cut.
        assert_eq!(
            display_preview_cut("ab\r\ncd\n", 80, 3),
            ("ab\ncd".into(), false)
        );
        assert_eq!(display_preview_cut("", 1, 1), (String::new(), false));
        // A preview that itself ends in an ellipsis is not cut either.
        assert_eq!(display_preview_cut("ab…", 80, 1), ("ab…".into(), false));
        assert_eq!(display_preview_cut("abcdef", 4, 1), ("abc…".into(), true));
        assert_eq!(display_preview_cut("ab\ncd", 80, 1), ("ab…".into(), true));
    }
}
