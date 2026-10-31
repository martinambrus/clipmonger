//! The popup window contents: rendering and key handling.

use crate::shell::Event;
use crate::{ipc_bridge, keymap, popup, strings};

use super::{now_ms, PopupApp};

impl PopupApp {
    /// Apply one view-model effect: shell events for the keys that pick,
    /// dismiss or refresh, bridge commands for the rest.
    pub(super) fn apply_effect(&mut self, effect: popup::VmEffect) {
        match effect {
            popup::VmEffect::None => {}
            popup::VmEffect::Paste(id) => self.feed(Event::Pick {
                id,
                picked_ms: now_ms(),
            }),
            popup::VmEffect::Dismiss => self.feed(Event::Dismiss),
            popup::VmEffect::Remove(id) => self.send(ipc_bridge::UiCmd::DeleteClip(id)),
            popup::VmEffect::Pin(id) => {
                let pinned = !self.vm.is_pinned(id);
                self.send(ipc_bridge::UiCmd::PinClip(id, pinned));
            }
            popup::VmEffect::Refresh => self.feed(Event::Refresh),
            popup::VmEffect::GroupTab(n) => {
                if let Ok(mut config) = self.config.lock() {
                    config
                        .perm_vm
                        .select_group(usize::from(n.saturating_sub(1)));
                    config.perm_source = Some(config.perm_vm.active_group);
                }
            }
            popup::VmEffect::ExecPermanent(id) => self.feed(Event::PickPermanent { id }),
        }
    }

    /// Forward special keys to the view-model; typing stays in the field.
    fn handle_keys(&mut self, ctx: &egui::Context, search_focused: bool) {
        if ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        }) {
            self.feed(Event::Key);
        }
        let widget_focused = !search_focused && ctx.memory(egui::Memory::focused).is_some();
        let pressed: Vec<(String, bool, bool)> = ctx.input(|input| {
            let mut out = Vec::new();
            let ctrl = input.modifiers.ctrl;
            let alt = input.modifiers.alt;
            let pairs = [
                (egui::Key::Enter, "Enter"),
                (egui::Key::Escape, "Escape"),
                (egui::Key::Delete, "Delete"),
                (egui::Key::Home, "Home"),
                (egui::Key::End, "End"),
                (egui::Key::PageUp, "PageUp"),
                (egui::Key::PageDown, "PageDown"),
                (egui::Key::ArrowUp, "ArrowUp"),
                (egui::Key::ArrowDown, "ArrowDown"),
                (egui::Key::F5, "F5"),
                (egui::Key::P, "p"),
                (egui::Key::E, "e"),
                (egui::Key::Num1, "1"),
                (egui::Key::Num2, "2"),
                (egui::Key::Num3, "3"),
                (egui::Key::Num4, "4"),
                (egui::Key::Num5, "5"),
                (egui::Key::Num6, "6"),
                (egui::Key::Num7, "7"),
                (egui::Key::Num8, "8"),
                (egui::Key::Num9, "9"),
            ];
            for (key, name) in pairs {
                // egui activates the focused widget without consuming its key.
                if widget_focused && matches!(key, egui::Key::Enter | egui::Key::Space) {
                    continue;
                }
                if input.key_pressed(key) {
                    out.push((name.to_string(), ctrl, alt));
                }
            }
            out
        });
        for (name, ctrl, alt) in pressed {
            let editing = search_focused && !ctrl && !alt;
            if editing && !popup::forwards_key_when_editing(&name) {
                continue;
            }
            let event = keymap::KeyEvent::new(name, ctrl, alt, false);
            let effect = self.vm.on_key(event);
            self.apply_effect(effect);
        }
    }

    /// Render the popup window contents.
    #[allow(clippy::too_many_lines)]
    pub(super) fn render(&mut self, ui: &mut egui::Ui) {
        let mut query = self.vm.query.clone();
        let mut clicked_row: Option<usize> = Option::None;
        let mut clicked_perm_row: Option<usize> = Option::None;
        let mut perm_tab_click: Option<Option<usize>> = None;
        let mut pin_row: Option<usize> = Option::None;
        let mut delete_row: Option<usize> = Option::None;
        let mut search_focused = false;
        let mut open_settings = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text(strings::text("popup.search_hint")),
            );
            if std::mem::take(&mut self.focus_search) {
                field.request_focus();
            }
            search_focused = field.has_focus();
            if let Some(current) = self.vm.current() {
                ui.label(strings::text("popup.current"));
                ui.label(strings::text(popup::preview_kind(current)));
                ui.label(&current.preview);
            }
            let group_names = self
                .config
                .lock()
                .map(|config| config.perm_vm.group_names())
                .unwrap_or_default();
            let perm_source = self.config.lock().map_or(None, |config| config.perm_source);
            if !group_names.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(perm_source.is_none(), strings::text("perm.tab_history"))
                        .clicked()
                    {
                        perm_tab_click = Some(None);
                    }
                    for (index, name) in group_names.iter().enumerate() {
                        if ui
                            .selectable_label(perm_source == Some(index), name)
                            .clicked()
                        {
                            perm_tab_click = Some(Some(index));
                        }
                    }
                });
            }
            if perm_source.is_some() && !group_names.is_empty() {
                let rows: Vec<(usize, String, bool)> = self
                    .config
                    .lock()
                    .map(|config| {
                        config
                            .perm_vm
                            .visible(&query)
                            .iter()
                            .enumerate()
                            .map(|(row, &index)| {
                                let name = config
                                    .perm_vm
                                    .permanents
                                    .get(index)
                                    .map_or_else(String::new, |clip| clip.name.clone());
                                (row, name, config.perm_vm.selected == row)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if rows.is_empty() {
                    ui.label(strings::text("popup.empty"));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (row, name, selected) in &rows {
                        if ui.selectable_label(*selected, name).clicked() {
                            clicked_perm_row = Some(*row);
                        }
                    }
                });
            } else {
                let count = self.vm.visible().len();
                if count == 0 {
                    ui.label(strings::text("popup.empty"));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for row in 0..count {
                        let Some(clip) = self.vm.clip_at(row) else {
                            continue;
                        };
                        let clip_id = clip.id;
                        let pinned = clip.pinned;
                        let response = ui.selectable_label(self.vm.selected == row, &clip.preview);
                        if response.clicked() {
                            clicked_row = Some(row);
                        }
                        response.context_menu(|ui| {
                            let pin_label = if pinned {
                                strings::text("popup.unpin")
                            } else {
                                strings::text("popup.pin")
                            };
                            if ui.button(pin_label).clicked() {
                                pin_row = Some(row);
                                ui.close();
                            }
                            if ui.button(strings::text("popup.delete")).clicked() {
                                delete_row = Some(row);
                                ui.close();
                            }
                            let _ = clip_id;
                        });
                    }
                });
            }
            if !self.vm.banner().is_empty() {
                ui.label(self.vm.banner());
            }
            ui.label(self.vm.status_line());
            if let Some(error) = self.shell.error() {
                ui.label(error);
            }
            if ui.button(strings::text("config.open_config")).clicked() {
                open_settings = true;
            }
        });
        if open_settings {
            self.open_settings(None);
        }
        if query != self.vm.query {
            self.vm.query = query;
            self.vm.selected = 0;
        }
        if let Some(tab) = perm_tab_click {
            match tab {
                None => {
                    if let Ok(mut config) = self.config.lock() {
                        config.perm_source = None;
                    }
                }
                Some(index) => {
                    if let Ok(mut config) = self.config.lock() {
                        config.perm_vm.select_group(index);
                        config.perm_source = Some(config.perm_vm.active_group);
                    }
                }
            }
        }
        if let Some(row) = clicked_perm_row {
            let activate = self.config.lock().map_or(None, |mut config| {
                if row == config.perm_vm.selected {
                    config.perm_vm.activate_selected()
                } else {
                    config.perm_vm.selected = row;
                    None
                }
            });
            if let Some(id) = activate {
                self.apply_effect(popup::VmEffect::ExecPermanent(id));
            }
        }
        if let Some(row) = pin_row {
            if let Some(clip) = self.vm.clip_at(row) {
                let id = clip.id;
                let pinned = !self.vm.is_pinned(id);
                self.send(ipc_bridge::UiCmd::PinClip(id, pinned));
            }
        }
        if let Some(row) = delete_row {
            if let Some(clip) = self.vm.clip_at(row) {
                let id = clip.id;
                self.send(ipc_bridge::UiCmd::DeleteClip(id));
            }
        }
        if let Some(row) = clicked_row {
            if row == self.vm.selected {
                let effect = self.vm.on_key(keymap::KeyEvent::new(
                    "Enter".to_string(),
                    false,
                    false,
                    false,
                ));
                self.apply_effect(effect);
            } else {
                self.vm.selected = row;
            }
        }
        self.handle_keys(ui.ctx(), search_focused);
    }
}
