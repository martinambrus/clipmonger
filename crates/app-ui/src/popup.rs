//! Popup view-model: clips, query, selection and banner state.

use clipmonger_core::model::Clip;
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
    current: Option<Clip>,
    offline: bool,
    target_valid: bool,
    paused: bool,
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
            current: Option::None,
            offline: false,
            target_valid: true,
            paused: false,
            keymap: keymap::Keymap::defaults(),
            target_session: target_session::TargetSession::new(),
        }
    }

    /// Replace the clip list and reset the selection.
    pub fn set_clips(&mut self, clips: Vec<Clip>) {
        self.clips = clips;
        self.selected = 0;
        self.clamp_selection();
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

    /// Mark monitoring paused or running for the status line.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Drop one clip after the server confirms the delete.
    pub fn remove_clip(&mut self, id: Uuid) {
        self.clips.retain(|clip| clip.id != id);
        self.clamp_selection();
    }

    /// Apply the server-confirmed pin flag to one clip.
    pub fn apply_pin(&mut self, id: Uuid, pinned: bool) {
        if let Some(clip) = self.clips.iter_mut().find(|clip| clip.id == id) {
            clip.pinned = pinned;
        }
    }

    /// Read the stored pin flag for one clip.
    #[must_use]
    pub fn is_pinned(&self, id: Uuid) -> bool {
        self.clips.iter().any(|clip| clip.id == id && clip.pinned)
    }

    /// Replace the list from a history-list reply body.
    pub fn apply_history(&mut self, body: &Value) -> bool {
        let Some(clips) = body.get("clips") else {
            return false;
        };
        let Ok(list) = serde_json::from_value::<Vec<Clip>>(clips.clone()) else {
            return false;
        };
        if self.clips.len() == list.len()
            && self
                .clips
                .iter()
                .zip(list.iter())
                .all(|(a, b)| a.id == b.id && a.preview == b.preview && a.pinned == b.pinned)
        {
            return false;
        }
        let prev_len = self.clips.len();
        self.clips = list;
        if self.clips.len() != prev_len {
            self.clamp_selection();
        }
        true
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
        search::filter_clips(&self.clips, &self.query)
    }

    /// Banner text for the current state, empty when all is well.
    #[must_use]
    pub fn banner(&self) -> &str {
        if self.offline {
            strings::text("popup.offline")
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
        } else {
            strings::text("popup.count").replace("{count}", &self.clips.len().to_string())
        }
    }

    /// Clamp the selection into the visible rows.
    fn clamp_selection(&mut self) {
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
                if !self.target_valid {
                    return VmEffect::None;
                }
                self.visible()
                    .get(self.selected)
                    .and_then(|index| self.clips.get(*index))
                    .map_or(VmEffect::None, |clip| VmEffect::Paste(clip.id))
            }
            keymap::Action::Dismiss => VmEffect::Dismiss,
            keymap::Action::Remove => self
                .visible()
                .get(self.selected)
                .and_then(|index| self.clips.get(*index))
                .map_or(VmEffect::None, |clip| VmEffect::Remove(clip.id)),
            keymap::Action::Pin => self
                .visible()
                .get(self.selected)
                .and_then(|index| self.clips.get(*index))
                .map_or(VmEffect::None, |clip| VmEffect::Pin(clip.id)),
            keymap::Action::Refresh => VmEffect::Refresh,
            keymap::Action::GroupTab(n) => VmEffect::GroupTab(n),
            keymap::Action::Edit | keymap::Action::None => VmEffect::None,
        }
    }
}
