//! Permanents view-model: groups, clips, selection and editor draft.

use clipmonger_core::model::{PermanentClip, PermanentKind};
use clipmonger_ipc::proto::methods::{PermanentCreateParams, PermanentGroupInfo};
use serde_json::Value;
use uuid::Uuid;

use crate::chord;

/// Editor working copy; `hotkey` is `""` when unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermanentDraft {
    /// Existing clip id, `None` for a new clip.
    pub id: Option<Uuid>,
    /// Group name.
    pub group: String,
    /// Clip name.
    pub name: String,
    /// Chord text, empty for none.
    pub hotkey: String,
    /// Paste body.
    pub body: String,
}

/// Pure permanents state for popup tabs and the config editor.
#[derive(Debug, Clone, Default)]
pub struct PermanentsVm {
    /// All loaded permanents in store order.
    pub permanents: Vec<PermanentClip>,
    /// Index into the non-system group list.
    pub active_group: usize,
    /// Selected row within the active group's visible rows.
    pub selected: usize,
    /// Editor working copy, when editing.
    pub draft: Option<PermanentDraft>,
    groups: Vec<PermanentGroupInfo>,
}

impl PermanentsVm {
    /// Build an empty VM.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Non-system groups in store order.
    fn visible_groups(&self) -> Vec<&PermanentGroupInfo> {
        self.groups.iter().filter(|g| !g.system).collect()
    }

    /// All group rows as loaded (including system groups).
    #[must_use]
    pub fn group_infos(&self) -> &[PermanentGroupInfo] {
        &self.groups
    }

    /// Replace permanents from a `permanent.list` reply body.
    pub fn apply_list(&mut self, body: &Value) -> bool {
        let Some(raw) = body.get("permanents") else {
            return false;
        };
        let Ok(list) = serde_json::from_value::<Vec<PermanentClip>>(raw.clone()) else {
            return false;
        };
        self.permanents = list;
        self.clamp();
        true
    }

    /// Replace groups from a `permanent.groups` reply body.
    pub fn apply_groups(&mut self, body: &Value) -> bool {
        let Some(raw) = body.get("groups") else {
            return false;
        };
        let Ok(list) = serde_json::from_value::<Vec<PermanentGroupInfo>>(raw.clone()) else {
            return false;
        };
        self.groups = list;
        self.clamp();
        true
    }

    /// Tab labels: non-system group names in store order.
    #[must_use]
    pub fn group_names(&self) -> Vec<String> {
        self.visible_groups()
            .iter()
            .map(|g| g.name.clone())
            .collect()
    }

    /// Indices into `permanents` for the active group, filtered by
    /// query over name and body (case-insensitive). System rows hidden.
    #[must_use]
    pub fn visible(&self, query: &str) -> Vec<usize> {
        let groups = self.visible_groups();
        let Some(active) = groups.get(self.active_group) else {
            return Vec::new();
        };
        let group_name = active.name.clone();
        let system_groups: Vec<&str> = self
            .groups
            .iter()
            .filter(|g| g.system)
            .map(|g| g.name.as_str())
            .collect();
        let q = query.to_lowercase();
        self.permanents
            .iter()
            .enumerate()
            .filter(|(_, clip)| {
                if clip.group != group_name
                    || clip.kind == PermanentKind::System
                    || system_groups.contains(&clip.group.as_str())
                {
                    return false;
                }
                if q.is_empty() {
                    return true;
                }
                clip.name.to_lowercase().contains(&q) || clip.body.to_lowercase().contains(&q)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Switch the active group, clamping and resetting selection.
    pub fn select_group(&mut self, n: usize) {
        let len = self.visible_groups().len();
        self.active_group = if len == 0 { 0 } else { n.min(len - 1) };
        self.selected = 0;
    }

    /// Id of the selected visible permanent, if any.
    #[must_use]
    pub fn activate_selected(&self) -> Option<Uuid> {
        self.visible("")
            .get(self.selected)
            .and_then(|index| self.permanents.get(*index))
            .map(|clip| clip.id)
    }

    /// Start a draft for a new clip in the active group.
    pub fn begin_new(&mut self) {
        let group = self
            .group_names()
            .get(self.active_group)
            .cloned()
            .unwrap_or_default();
        self.draft = Some(PermanentDraft {
            id: None,
            group,
            name: String::new(),
            hotkey: String::new(),
            body: String::new(),
        });
    }

    /// Start a draft editing the clip with `id`, if loaded.
    pub fn begin_edit(&mut self, id: Uuid) {
        if let Some(clip) = self.permanents.iter().find(|c| c.id == id) {
            self.draft = Some(PermanentDraft {
                id: Some(id),
                group: clip.group.clone(),
                name: clip.name.clone(),
                hotkey: clip.hotkey.clone().unwrap_or_default(),
                body: clip.body.clone(),
            });
        }
    }

    /// Validate the draft into create params. Errors are string ids.
    ///
    /// # Errors
    ///
    /// Returns a `strings` id for empty name/group/body, a bad chord,
    /// or a hotkey already used by another permanent.
    pub fn validate_draft(&self) -> Result<PermanentCreateParams, &'static str> {
        let Some(draft) = &self.draft else {
            return Err("perm.invalid_name");
        };
        if draft.name.trim().is_empty() {
            return Err("perm.invalid_name");
        }
        if draft.group.trim().is_empty() {
            return Err("perm.invalid_group");
        }
        if draft.body.is_empty() {
            return Err("perm.invalid_body");
        }
        let hotkey = if draft.hotkey.trim().is_empty() {
            None
        } else {
            let parsed =
                chord::parse_chord(draft.hotkey.trim()).map_err(|_| "config.invalid_chord")?;
            let normalised = chord::format_chord(&parsed);
            let conflict = self.permanents.iter().any(|clip| {
                if Some(clip.id) == draft.id {
                    return false;
                }
                clip.hotkey.as_deref().is_some_and(|other| {
                    chord::parse_chord(other).map_or_else(
                        |_| other == normalised,
                        |ev| chord::format_chord(&ev) == normalised,
                    )
                })
            });
            if conflict {
                return Err("perm.hotkey_conflict");
            }
            Some(normalised)
        };
        Ok(PermanentCreateParams {
            group: draft.group.trim().to_string(),
            name: draft.name.trim().to_string(),
            hotkey,
            body: draft.body.clone(),
        })
    }

    /// Drop one clip after the server confirms the delete.
    pub fn remove_confirmed(&mut self, id: Uuid) {
        self.permanents.retain(|clip| clip.id != id);
        self.clamp();
    }

    /// Clamp group and selection indices into range.
    fn clamp(&mut self) {
        let groups = self.visible_groups().len();
        if groups == 0 {
            self.active_group = 0;
        } else if self.active_group >= groups {
            self.active_group = groups - 1;
        }
        let visible = self.visible("").len();
        if visible == 0 {
            self.selected = 0;
        } else if self.selected >= visible {
            self.selected = visible - 1;
        }
    }
}
