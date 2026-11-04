//! The Settings window: the state it shares with the popup and its egui body.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use serde_json::Value;

use crate::{config_model, ipc_bridge, permanents, strings};

pub(crate) const DEFAULT_DARK: bool = true;
pub(crate) const DEFAULT_FONT_SCALE: f32 = 1.0;

/// State of the Settings window, shared with the popup (theme, permanents).
pub struct ConfigWindowState {
    pub doc: Option<config_model::ConfigDoc>,
    pub filter: String,
    pub tab: String,
    pub edits: std::collections::HashMap<String, String>,
    pub error: String,
    pub caps_text: String,
    /// When a popup-hotkey change applies (from the daemon caps).
    pub hotkey_apply: config_model::HotkeyApply,
    pub version_text: String,
    pub profile: PathBuf,
    pub backup_path: String,
    pub log_lines: Vec<String>,
    pub log_source: String,
    pub log_error: String,
    pub dark: bool,
    pub font_scale: f32,
    pub pending_apply: Vec<String>,
    pub restore_pending: bool,
    pub(crate) restore_notice: Option<String>,
    pub perm_vm: permanents::PermanentsVm,
    pub perm_source: Option<usize>,
    pub perm_group_new: String,
    pub perm_confirm_delete: Option<uuid::Uuid>,
    /// The daemon runs portable, where Start on login is unavailable.
    pub autostart_portable: bool,
    /// Per-setting error text, drawn under the setting's control.
    pub field_errors: std::collections::HashMap<String, String>,
    /// Setting whose text field takes keyboard focus on the next frame.
    pub focus_field: Option<String>,
}

impl ConfigWindowState {
    /// Switch streams without displaying replies from the previous selection.
    pub fn select_log_source(&mut self, source: &str) {
        if matches!(source, "audit" | "daemon" | "ui") && self.log_source != source {
            self.log_source = source.to_string();
            self.log_lines.clear();
            self.log_error.clear();
        }
    }

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
            hotkey_apply: config_model::HotkeyApply::Live,
            version_text: String::new(),
            profile,
            backup_path,
            log_lines: Vec::new(),
            log_source: "audit".to_string(),
            log_error: String::new(),
            dark: DEFAULT_DARK,
            font_scale: DEFAULT_FONT_SCALE,
            pending_apply: Vec::new(),
            restore_pending: false,
            restore_notice: None,
            perm_vm: permanents::PermanentsVm::new(),
            perm_source: None,
            perm_group_new: String::new(),
            perm_confirm_delete: None,
            autostart_portable: false,
            field_errors: std::collections::HashMap::new(),
            focus_field: None,
        }
    }
}

/// One rendered settings row: group heading id (first row of a filtered
/// group only), key, kind, label, current text, dirty.
type SettingsRow<'a> = (
    Option<&'a str>,
    String,
    config_model::Kind,
    String,
    String,
    bool,
);

/// Settings tabs in display order, with their heading string ids.
const SETTINGS_TABS: &[(config_model::Tab, &str)] = &[
    (config_model::Tab::History, "config.tab_history"),
    (config_model::Tab::Popup, "config.tab_popup"),
    (config_model::Tab::Tray, "config.tab_tray"),
    (config_model::Tab::Startup, "config.tab_startup"),
    (config_model::Tab::Privacy, "config.tab_privacy"),
    (config_model::Tab::Advanced, "config.tab_advanced"),
    (
        config_model::Tab::PermanentClips,
        "config.tab_permanent_clips",
    ),
    (config_model::Tab::General, "config.tab_general"),
];

/// Render the action row identically for measurement and interaction.
fn config_actions(ui: &mut egui::Ui, dirty: bool) -> (bool, bool) {
    ui.horizontal(|ui| {
        let apply = ui
            .add_enabled(dirty, egui::Button::new(strings::text("config.apply")))
            .clicked();
        let revert = ui
            .add_enabled(dirty, egui::Button::new(strings::text("config.revert")))
            .clicked();
        (apply, revert)
    })
    .inner
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
    let mut invalid_edits: Vec<String> = Vec::new();
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
        if config.restore_pending {
            ui.disable();
        }
        ui.add(
            egui::TextEdit::singleline(&mut config.filter)
                .hint_text(strings::text("config.filter_hint")),
        );
        // A filter searches the settings of every tab, grouped by tab.
        let filtering = !config.filter.is_empty();
        let filter_lc = config.filter.to_lowercase();
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
                    .selectable_label(!filtering && config.tab == *name, strings::text(label))
                    .clicked()
                {
                    config.tab = (*name).to_string();
                    config.filter.clear();
                }
            }
        });
        if !config.error.is_empty() {
            ui.label(&config.error);
            ui.separator();
        }
        if config.tab == "databases" && !filtering {
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
            if ui
                .add_enabled(
                    !config.restore_pending,
                    egui::Button::new(strings::text("config.backup_restore")),
                )
                .clicked()
            {
                do_restore = true;
            }
        } else if config.tab == "about" && !filtering {
            ui.label(strings::text("config.help_title"));
            ui.label(strings::text("config.about_version"));
            ui.label(&config.version_text);
            ui.label(strings::text("config.accesskit_note"));
            ui.label(strings::text("config.caps_title"));
            ui.label(&config.caps_text);
            ui.label(strings::text("config.logs_title"));
            ui.horizontal(|ui| {
                ui.label(strings::text("config.logs_source"));
                for src in ["audit", "daemon", "ui"] {
                    let selected = config.log_source == src;
                    if ui.selectable_label(selected, src).clicked() && !selected {
                        config.select_log_source(src);
                        do_logs = true;
                    }
                }
                if ui.button(strings::text("config.logs_refresh")).clicked() {
                    do_logs = true;
                }
            });
            if config.log_source == "ui" {
                if let Some(error) = clipmonger_core::runtime_log::global_status(
                    clipmonger_core::runtime_log::Role::Ui,
                ) {
                    ui.label(format!("ui sink: {error}"));
                }
            }
            if !config.log_error.is_empty() {
                ui.label(format!("{}: {}", config.log_source, config.log_error));
            } else if !config.log_lines.is_empty() {
                ui.label(format!(
                    "{} ({} lines)",
                    config.log_source,
                    config.log_lines.len()
                ));
            }
            for line in &config.log_lines {
                ui.label(line);
            }
        } else {
            // Keep config actions in view even when the permanent editor is long.
            // Measure the actual buttons without interaction, as the popup does.
            let dirty = config
                .doc
                .as_ref()
                .is_some_and(|doc| !doc.dirty_keys().is_empty());
            let full_rect = ui.available_rect_before_wrap();
            let action_height = {
                let mut measure = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("config_actions_measure")
                        .sizing_pass()
                        .invisible(),
                );
                config_actions(&mut measure, dirty);
                measure.min_size().y
            };
            let body_height =
                (full_rect.height() - action_height - ui.spacing().item_spacing.y).max(0.0);
            egui::ScrollArea::vertical()
                .id_salt("config_body")
                .max_height(body_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let matches = |id: &str| {
                        !filtering || strings::text(id).to_lowercase().contains(&filter_lc)
                    };
                    // Theme controls persist through the ui.theme extra key.
                    if matches("config.theme_dark")
                        && ui
                            .checkbox(&mut config.dark, strings::text("config.theme_dark"))
                            .changed()
                    {
                        theme_changed = true;
                    }
                    if matches("config.font_scale") {
                        let scale = egui::Slider::new(&mut config.font_scale, 0.8..=1.6)
                            .text(strings::text("config.font_scale"));
                        if ui.add(scale).changed() {
                            theme_changed = true;
                        }
                    }
                    ui.label(strings::text("config.non_atomic_note"));
                    if config.tab == "PermanentClips" && !filtering {
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
                    let active_tab = match config.tab.as_str() {
                        "History" => Some(config_model::Tab::History),
                        "Popup" => Some(config_model::Tab::Popup),
                        "Advanced" => Some(config_model::Tab::Advanced),
                        "PermanentClips" => Some(config_model::Tab::PermanentClips),
                        "Privacy" => Some(config_model::Tab::Privacy),
                        "Tray" => Some(config_model::Tab::Tray),
                        "Startup" => Some(config_model::Tab::Startup),
                        "General" => Some(config_model::Tab::General),
                        _ => None,
                    };
                    let filtered_set: std::collections::HashSet<usize> =
                        filtered.into_iter().collect();
                    let tab_rows = |tab: config_model::Tab| {
                        config_model::visible_rows(tab)
                            .into_iter()
                            .filter(|idx| filtered_set.contains(idx))
                            .collect::<Vec<_>>()
                    };
                    // (tab heading id, row indices); headings only while filtering.
                    let groups: Vec<(Option<&str>, Vec<usize>)> = if filtering {
                        SETTINGS_TABS
                            .iter()
                            .map(|(tab, label)| (Some(*label), tab_rows(*tab)))
                            .filter(|(_, rows)| !rows.is_empty())
                            .collect()
                    } else if let Some(tab) = active_tab {
                        vec![(None, tab_rows(tab))]
                    } else {
                        Vec::new()
                    };
                    let mut rows: Vec<SettingsRow<'_>> = Vec::new();
                    for (index, heading) in groups.iter().flat_map(|(heading, rows)| {
                        rows.iter()
                            .enumerate()
                            .map(move |(n, index)| (*index, heading.filter(|_| n == 0)))
                    }) {
                        let row = config_model::ROWS[index];
                        let saved = doc.get(row.key);
                        let staged = doc.staged(row.key);
                        let current = staged.or(saved);
                        let text = current.map_or_else(String::new, |value| {
                            config_model::value_text(row.kind, value)
                        });
                        rows.push((
                            heading,
                            row.key.to_string(),
                            row.kind,
                            strings::text(row.label).to_string(),
                            text,
                            doc.is_dirty(row.key),
                        ));
                        if row.key == "popup.hotkey" {}
                    }
                    let focus_field = config.focus_field.take();
                    for (heading, key, kind, label, current, _dirty) in &rows {
                        if let Some(heading) = heading {
                            ui.heading(strings::text(heading));
                        }
                        ui.label(label);
                        if *kind == config_model::Kind::Bool {
                            let mut flag = current == "true";
                            let portable = key == "autostart.enabled" && config.autostart_portable;
                            if ui
                                .add_enabled(
                                    !portable,
                                    egui::Checkbox::new(&mut flag, label.as_str()),
                                )
                                .changed()
                            {
                                stage_edits.push((key.clone(), Value::Bool(flag)));
                            }
                            if portable {
                                ui.label(strings::text("config.restore_warning_portable"));
                            }
                        } else if *kind == config_model::Kind::Enum {
                            let mut picked = current.clone();
                            egui::ComboBox::from_id_salt(("cfg_enum", key.as_str()))
                                .selected_text(current.as_str())
                                .show_ui(ui, |ui| {
                                    for choice in config_model::enum_choices(key) {
                                        ui.selectable_value(
                                            &mut picked,
                                            (*choice).to_string(),
                                            *choice,
                                        );
                                    }
                                });
                            if picked != *current {
                                stage_edits.push((key.clone(), Value::String(picked)));
                            }
                        } else {
                            let entry = config
                                .edits
                                .entry(key.clone())
                                .or_insert_with(|| current.clone());
                            // A stable id keeps focus when rows above change.
                            let response = ui.add(
                                egui::TextEdit::singleline(entry).id_salt(("cfg", key.as_str())),
                            );
                            if response.changed() {
                                match config_model::parse_edit(*kind, entry) {
                                    Some(value) => stage_edits.push((key.clone(), value)),
                                    None => invalid_edits.push(key.clone()),
                                }
                            }
                            if focus_field.as_deref() == Some(key.as_str()) {
                                response.request_focus();
                            }
                        }
                        if let Some(error) = config.field_errors.get(key) {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        if key == "popup.hotkey"
                            && config.hotkey_apply == config_model::HotkeyApply::OnRestart
                        {
                            ui.label(strings::text("config.applies_on_restart"));
                        }
                    }
                });
            let mut actions =
                ui.new_child(egui::UiBuilder::new().id_salt("config_actions").max_rect(
                    egui::Rect::from_min_max(
                        egui::pos2(full_rect.left(), full_rect.bottom() - action_height),
                        full_rect.max,
                    ),
                ));
            (do_apply, do_revert) = config_actions(&mut actions, dirty);
            ui.expand_to_include_rect(actions.min_rect());
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
    if theme_changed && !config.restore_pending && !do_restore {
        let mode = if config.dark { "dark" } else { "light" };
        send(ipc_bridge::UiCmd::ConfigSet(
            "ui.theme".to_string(),
            serde_json::json!({ "mode": mode, "font_scale": config.font_scale }),
        ));
    }
    let accept_edits = !config.restore_pending && !do_restore;
    for (key, value) in stage_edits.into_iter().filter(|_| accept_edits) {
        if let Some(doc) = &mut config.doc {
            match doc.stage(&key, value) {
                Ok(()) => {
                    config.error.clear();
                    config.field_errors.remove(&key);
                }
                Err(id) => {
                    // A rejected value must not leave an older one staged.
                    doc.unstage(&key);
                    config
                        .field_errors
                        .insert(key, strings::text(&id).to_string());
                }
            }
        }
    }
    for key in invalid_edits.into_iter().filter(|_| accept_edits) {
        if let Some(doc) = &mut config.doc {
            doc.unstage(&key);
        }
        config
            .field_errors
            .insert(key, strings::text("config.invalid_value").to_string());
    }
    if do_purge {
        send(ipc_bridge::UiCmd::PurgeRemoved);
    }
    if do_backup {
        send(ipc_bridge::UiCmd::BackupCreate(config.backup_path.clone()));
    }
    if do_restore && !config.restore_pending {
        config.restore_pending = true;
        config.restore_notice = None;
        config.pending_apply.clear();
        config.edits.clear();
        config.field_errors.clear();
        config.doc = None;
        send(ipc_bridge::UiCmd::BackupRestore(config.backup_path.clone()));
    }
    if do_logs {
        let src = config.log_source.clone();
        if src == "audit" {
            send(ipc_bridge::UiCmd::LogsTail(100));
        } else {
            send(ipc_bridge::UiCmd::LogsTailSource {
                lines: 100,
                source: src,
            });
        }
    }
    if do_revert && !config.restore_pending {
        if let Some(doc) = &mut config.doc {
            doc.revert();
        }
        config.edits.clear();
        config.field_errors.clear();
    }
    if do_apply && !config.restore_pending {
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
