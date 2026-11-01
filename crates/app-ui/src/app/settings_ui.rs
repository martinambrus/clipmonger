//! The Settings window: the state it shares with the popup and its egui body.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use serde_json::Value;

use crate::{config_model, ipc_bridge, permanents, strings};

/// State of the Settings window, shared with the popup (theme, permanents).
pub struct ConfigWindowState {
    pub doc: Option<config_model::ConfigDoc>,
    pub filter: String,
    pub tab: String,
    pub edits: std::collections::HashMap<String, String>,
    pub error: String,
    pub caps_text: String,
    pub version_text: String,
    pub profile: PathBuf,
    pub backup_path: String,
    pub log_lines: Vec<String>,
    pub dark: bool,
    pub font_scale: f32,
    pub pending_apply: Vec<String>,
    pub perm_vm: permanents::PermanentsVm,
    pub perm_source: Option<usize>,
    pub perm_group_new: String,
    pub perm_confirm_delete: Option<uuid::Uuid>,
}

impl ConfigWindowState {
    /// Fresh state for the profile.
    #[must_use]
    pub fn new(profile: PathBuf) -> Self {
        let backup_path = profile.join("backup.zip").display().to_string();
        Self {
            doc: None,
            filter: String::new(),
            tab: "History".to_string(),
            edits: std::collections::HashMap::new(),
            error: String::new(),
            caps_text: String::new(),
            version_text: String::new(),
            profile,
            backup_path,
            log_lines: Vec::new(),
            dark: true,
            font_scale: 1.0,
            pending_apply: Vec::new(),
            perm_vm: permanents::PermanentsVm::new(),
            perm_source: None,
            perm_group_new: String::new(),
            perm_confirm_delete: None,
        }
    }
}

/// Render the Settings window body.
#[allow(clippy::too_many_lines)]
pub fn render_config_viewport(
    ui: &mut egui::Ui,
    state: &Arc<Mutex<ConfigWindowState>>,
    cmds: &mpsc::Sender<ipc_bridge::UiCmd>,
) {
    let Ok(mut config) = state.lock() else {
        return;
    };
    let mut do_apply = false;
    let mut do_revert = false;
    let mut do_purge = false;
    let mut do_backup = false;
    let mut do_restore = false;
    let mut do_logs = false;
    let mut stage_edits: Vec<(String, Value)> = Vec::new();
    let mut theme_changed = false;
    let mut perm_send: Vec<ipc_bridge::UiCmd> = Vec::new();
    let mut perm_save = false;
    let mut perm_cancel = false;
    let mut perm_begin_new = false;
    let mut perm_begin_edit: Option<uuid::Uuid> = None;
    let mut perm_select: Option<usize> = None;
    let mut perm_confirm: Option<uuid::Uuid> = None;
    let send = |cmd: ipc_bridge::UiCmd| {
        let _ = cmds.send(cmd);
    };
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut config.filter)
                .hint_text(strings::text("config.filter_hint")),
        );
        let tabs: &[(&str, &str)] = &[
            ("History", "config.tab_history"),
            ("Popup", "config.tab_popup"),
            ("Tray", "config.tab_tray"),
            ("Startup", "config.tab_startup"),
            ("Privacy", "config.tab_privacy"),
            ("Advanced", "config.tab_advanced"),
            ("PermanentClips", "config.tab_permanent_clips"),
            ("General", "config.tab_general"),
            ("databases", "config.tab_databases"),
            ("about", "config.tab_about"),
        ];
        ui.horizontal_wrapped(|ui| {
            for (name, label) in tabs {
                if ui
                    .selectable_label(config.tab == *name, strings::text(label))
                    .clicked()
                {
                    config.tab = (*name).to_string();
                }
            }
        });
        if !config.error.is_empty() {
            ui.label(&config.error);
            ui.separator();
        }
        if config.tab == "databases" {
            ui.label(strings::text("config.profile_path"));
            ui.label(config.profile.display().to_string());
            if ui.button(strings::text("config.purge_removed")).clicked() {
                do_purge = true;
            }
            ui.label(strings::text("config.backup_path"));
            ui.add(egui::TextEdit::singleline(&mut config.backup_path));
            if ui.button(strings::text("config.backup_create")).clicked() {
                do_backup = true;
            }
            if ui.button(strings::text("config.backup_restore")).clicked() {
                do_restore = true;
            }
        } else if config.tab == "about" {
            ui.label(strings::text("config.help_title"));
            ui.label(strings::text("config.about_version"));
            ui.label(&config.version_text);
            ui.label(strings::text("config.accesskit_note"));
            ui.label(strings::text("config.caps_title"));
            ui.label(&config.caps_text);
            ui.label(strings::text("config.logs_title"));
            if ui.button(strings::text("config.logs_refresh")).clicked() {
                do_logs = true;
            }
            for line in &config.log_lines {
                ui.label(line);
            }
        } else {
            // Theme controls persist through the ui.theme extra key.
            if ui
                .checkbox(&mut config.dark, strings::text("config.theme_dark"))
                .changed()
            {
                theme_changed = true;
            }
            let scale = egui::Slider::new(&mut config.font_scale, 0.8..=1.6)
                .text(strings::text("config.font_scale"));
            if ui.add(scale).changed() {
                theme_changed = true;
            }
            ui.label(strings::text("config.non_atomic_note"));
            if config.tab == "PermanentClips" {
                ui.label(strings::text("perm.immediate_note"));
                ui.label(strings::text("perm.hotkey_note"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut config.perm_group_new));
                    if ui.button(strings::text("perm.new_group")).clicked() {
                        perm_send.push(ipc_bridge::UiCmd::PermGroupCreate(
                            config.perm_group_new.clone(),
                        ));
                    }
                });
                let group_names = config.perm_vm.group_names();
                for (index, name) in group_names.iter().enumerate() {
                    if ui
                        .selectable_label(config.perm_vm.active_group == index, name)
                        .clicked()
                    {
                        perm_select = Some(index);
                    }
                }
                let active_count = config
                    .perm_vm
                    .group_infos()
                    .iter()
                    .find(|g| group_names.get(config.perm_vm.active_group) == Some(&g.name))
                    .map_or(0, |g| g.count);
                if active_count > 0 {
                    ui.label(strings::text("perm.group_not_empty"));
                }
                if let Some(name) = group_names.get(config.perm_vm.active_group).cloned() {
                    if ui
                        .add_enabled(
                            active_count == 0,
                            egui::Button::new(strings::text("perm.delete_group")),
                        )
                        .clicked()
                    {
                        perm_send.push(ipc_bridge::UiCmd::PermGroupDelete(name));
                    }
                }
                let visible_rows: Vec<(uuid::Uuid, String, String)> = config
                    .perm_vm
                    .visible("")
                    .iter()
                    .map(|&i| {
                        let clip = &config.perm_vm.permanents[i];
                        (
                            clip.id,
                            clip.name.clone(),
                            clip.hotkey.clone().unwrap_or_default(),
                        )
                    })
                    .collect();
                if visible_rows.is_empty() {
                    ui.label(strings::text("perm.empty_group"));
                }
                for (row, (id, name, hotkey)) in visible_rows.iter().enumerate() {
                    let label = if hotkey.is_empty() {
                        name.clone()
                    } else {
                        format!("{name} {hotkey}")
                    };
                    if ui
                        .selectable_label(config.perm_vm.selected == row, label)
                        .clicked()
                    {
                        config.perm_vm.selected = row;
                        perm_begin_edit = Some(*id);
                    }
                }
                if ui.button(strings::text("perm.new_clip")).clicked() {
                    perm_begin_new = true;
                }
                if let Some(id) = config.perm_vm.activate_selected() {
                    if ui.button(strings::text("perm.delete_clip")).clicked() {
                        perm_confirm = Some(id);
                    }
                    if config.perm_confirm_delete == Some(id) {
                        ui.label(strings::text("perm.delete_confirm"));
                        if ui.button(strings::text("perm.delete_clip")).clicked() {
                            perm_send.push(ipc_bridge::UiCmd::PermDelete(id));
                        }
                    }
                }
                if let Some(draft) = &mut config.perm_vm.draft {
                    ui.label(strings::text("perm.name"));
                    ui.add(egui::TextEdit::singleline(&mut draft.name));
                    ui.label(strings::text("perm.group"));
                    ui.add(egui::TextEdit::singleline(&mut draft.group));
                    ui.label(strings::text("perm.hotkey"));
                    ui.add(egui::TextEdit::singleline(&mut draft.hotkey));
                    ui.label(strings::text("perm.body"));
                    ui.add(egui::TextEdit::multiline(&mut draft.body));
                    if ui.button(strings::text("perm.save")).clicked() {
                        perm_save = true;
                    }
                    if ui.button(strings::text("perm.cancel_edit")).clicked() {
                        perm_cancel = true;
                    }
                }
            }
            let Some(doc) = &config.doc else {
                ui.label(strings::text("popup.offline"));
                return;
            };
            let filtered = config_model::filter_rows(&config.filter);
            let mut rows: Vec<(String, config_model::Kind, String, String, bool)> = Vec::new();
            for index in filtered {
                let row = config_model::ROWS[index];
                if format!("{:?}", row.tab) != config.tab {
                    continue;
                }
                if config.tab == "PermanentClips" && row.key == "permanents.groups" {
                    continue;
                }
                let saved = doc.get(row.key);
                let staged = doc.staged(row.key);
                let current = staged.or(saved);
                let text = current.map_or_else(String::new, |value| {
                    config_model::value_text(row.kind, value)
                });
                rows.push((
                    row.key.to_string(),
                    row.kind,
                    strings::text(row.label).to_string(),
                    text,
                    doc.is_dirty(row.key),
                ));
                if row.key == "popup.hotkey" {}
            }
            let dirty = doc.dirty_keys();
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .show(ui, |ui| {
                    for (key, kind, label, current, _dirty) in &rows {
                        ui.label(label);
                        if *kind == config_model::Kind::Bool {
                            let mut flag = current == "true";
                            if ui.checkbox(&mut flag, label.as_str()).changed() {
                                stage_edits.push((key.clone(), Value::Bool(flag)));
                            }
                        } else {
                            let entry = config
                                .edits
                                .entry(key.clone())
                                .or_insert_with(|| current.clone());
                            if ui.add(egui::TextEdit::singleline(entry)).changed() {
                                if let Some(value) = config_model::parse_edit(*kind, entry) {
                                    stage_edits.push((key.clone(), value));
                                }
                            }
                        }
                        // The Windows daemon applies a new chord live.
                        #[cfg(not(windows))]
                        if key == "popup.hotkey" {
                            ui.label(strings::text("config.applies_on_restart"));
                        }
                    }
                });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !dirty.is_empty(),
                        egui::Button::new(strings::text("config.apply")),
                    )
                    .clicked()
                {
                    do_apply = true;
                }
                if ui
                    .add_enabled(
                        !dirty.is_empty(),
                        egui::Button::new(strings::text("config.revert")),
                    )
                    .clicked()
                {
                    do_revert = true;
                }
            });
        }
    });
    if let Some(index) = perm_select {
        config.perm_vm.select_group(index);
    }
    if perm_begin_new {
        config.perm_vm.begin_new();
    }
    if let Some(id) = perm_begin_edit {
        config.perm_vm.begin_edit(id);
    }
    if let Some(id) = perm_confirm {
        config.perm_confirm_delete = Some(id);
    }
    if perm_cancel {
        config.perm_vm.draft = None;
    }
    if perm_save {
        match config.perm_vm.validate_draft() {
            Ok(params) => {
                config.error.clear();
                let draft_id = config.perm_vm.draft.as_ref().and_then(|d| d.id);
                if let Some(id) = draft_id {
                    if let Some(mut clip) = config
                        .perm_vm
                        .permanents
                        .iter()
                        .find(|c| c.id == id)
                        .cloned()
                    {
                        clip.group = params.group;
                        clip.name = params.name;
                        clip.hotkey = params.hotkey;
                        clip.body = params.body;
                        send(ipc_bridge::UiCmd::PermUpdate(clip));
                    }
                } else {
                    send(ipc_bridge::UiCmd::PermCreate(params));
                }
            }
            Err(id) => config.error = strings::text(id).to_string(),
        }
    }
    for cmd in perm_send {
        send(cmd);
    }
    if theme_changed {
        let mode = if config.dark { "dark" } else { "light" };
        send(ipc_bridge::UiCmd::ConfigSet(
            "ui.theme".to_string(),
            serde_json::json!({ "mode": mode, "font_scale": config.font_scale }),
        ));
    }
    for (key, value) in stage_edits {
        if let Some(doc) = &mut config.doc {
            match doc.stage(&key, value) {
                Ok(()) => config.error.clear(),
                Err(id) => config.error = strings::text(&id).to_string(),
            }
        }
    }
    if do_purge {
        send(ipc_bridge::UiCmd::PurgeRemoved);
    }
    if do_backup {
        send(ipc_bridge::UiCmd::BackupCreate(config.backup_path.clone()));
    }
    if do_restore {
        send(ipc_bridge::UiCmd::BackupRestore(config.backup_path.clone()));
    }
    if do_logs {
        send(ipc_bridge::UiCmd::LogsTail(100));
    }
    if do_revert {
        if let Some(doc) = &mut config.doc {
            doc.revert();
        }
        config.edits.clear();
    }
    if do_apply {
        if let Some(doc) = config.doc.as_ref() {
            let pending = doc.dirty_keys();
            let plan = doc.apply_plan();
            config.pending_apply = pending;
            for (key, value) in plan {
                let wire_key = if key == "popup.keys.*" {
                    "popup.keys".to_string()
                } else {
                    key
                };
                send(ipc_bridge::UiCmd::ConfigSet(wire_key, value));
            }
            // popup.keys live-applies in the Config event handler on reload.
            send(ipc_bridge::UiCmd::ConfigGet);
        }
    }
}
