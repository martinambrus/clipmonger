//! The popup window contents: rendering and key handling.

use crate::shell::Event;
use crate::{ipc_bridge, keymap, popup, strings};

use super::{now_ms, PopupApp};

/// Width in points around a preview line: panel margins, scroll bar and the
/// image thumbnail slot.
const FIT_MARGIN: f32 = 72.0;

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

    pub(super) fn egui_key_to_chord_name(key: egui::Key) -> Option<String> {
        let name = match key {
            egui::Key::ArrowUp => "ArrowUp",
            egui::Key::ArrowDown => "ArrowDown",
            egui::Key::ArrowLeft => "ArrowLeft",
            egui::Key::ArrowRight => "ArrowRight",
            egui::Key::Home => "Home",
            egui::Key::End => "End",
            egui::Key::PageUp => "PageUp",
            egui::Key::PageDown => "PageDown",
            egui::Key::Enter => "Enter",
            egui::Key::Escape => "Escape",
            egui::Key::Delete => "Delete",
            egui::Key::Backspace => "Backspace",
            egui::Key::Tab => "Tab",
            egui::Key::Space => "Space",
            egui::Key::A => "a",
            egui::Key::B => "b",
            egui::Key::C => "c",
            egui::Key::D => "d",
            egui::Key::E => "e",
            egui::Key::F => "f",
            egui::Key::G => "g",
            egui::Key::H => "h",
            egui::Key::I => "i",
            egui::Key::J => "j",
            egui::Key::K => "k",
            egui::Key::L => "l",
            egui::Key::M => "m",
            egui::Key::N => "n",
            egui::Key::O => "o",
            egui::Key::P => "p",
            egui::Key::Q => "q",
            egui::Key::R => "r",
            egui::Key::S => "s",
            egui::Key::T => "t",
            egui::Key::U => "u",
            egui::Key::V => "v",
            egui::Key::W => "w",
            egui::Key::X => "x",
            egui::Key::Y => "y",
            egui::Key::Z => "z",
            egui::Key::Num0 => "0",
            egui::Key::Num1 => "1",
            egui::Key::Num2 => "2",
            egui::Key::Num3 => "3",
            egui::Key::Num4 => "4",
            egui::Key::Num5 => "5",
            egui::Key::Num6 => "6",
            egui::Key::Num7 => "7",
            egui::Key::Num8 => "8",
            egui::Key::Num9 => "9",
            egui::Key::F1 => "F1",
            egui::Key::F2 => "F2",
            egui::Key::F3 => "F3",
            egui::Key::F4 => "F4",
            egui::Key::F5 => "F5",
            egui::Key::F6 => "F6",
            egui::Key::F7 => "F7",
            egui::Key::F8 => "F8",
            egui::Key::F9 => "F9",
            egui::Key::F10 => "F10",
            egui::Key::F11 => "F11",
            egui::Key::F12 => "F12",
            egui::Key::F13 => "F13",
            egui::Key::F14 => "F14",
            egui::Key::F15 => "F15",
            egui::Key::F16 => "F16",
            egui::Key::F17 => "F17",
            egui::Key::F18 => "F18",
            egui::Key::F19 => "F19",
            egui::Key::F20 => "F20",
            egui::Key::F21 => "F21",
            egui::Key::F22 => "F22",
            egui::Key::F23 => "F23",
            egui::Key::F24 => "F24",
            _ => return None,
        };
        Some(name.to_string())
    }

    /// Forward special keys to the view-model; typing stays in the field.
    fn handle_keys(&mut self, ctx: &egui::Context, search_focused: bool, events: &[egui::Event]) {
        if events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        {
            self.feed(Event::Key);
        }
        let widget_focused = !search_focused && ctx.memory(|m| m.focused().is_some());
        let pressed: Vec<(String, bool, bool, bool, bool)> = {
            let mut out = Vec::new();
            for event in events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                {
                    if ctx.text_edit_focused()
                        && modifiers.command_only()
                        && matches!(key, egui::Key::C | egui::Key::X | egui::Key::V)
                    {
                        continue;
                    }
                    if widget_focused && (*key == egui::Key::Enter || *key == egui::Key::Space) {
                        continue;
                    }
                    let Some(name) = Self::egui_key_to_chord_name(*key) else {
                        continue;
                    };
                    let ctrl = modifiers.ctrl;
                    let alt = modifiers.alt;
                    let shift = modifiers.shift;
                    let super_key = modifiers.mac_cmd;
                    out.push((name, ctrl, alt, shift, super_key));
                }
            }
            out
        };
        for (name, ctrl, alt, shift, super_key) in pressed {
            let editing = search_focused && !ctrl && !alt && !super_key;
            if editing && !popup::forwards_key_when_editing(&name) && !name.starts_with('F') {
                continue;
            }
            let event = keymap::KeyEvent {
                key: name.clone(),
                ctrl,
                alt,
                shift,
                super_key,
            };
            let permanent = self.config.lock().is_ok_and(|c| c.perm_source.is_some());
            if name == "Backspace" && !ctrl && !alt && !super_key {
                self.vm.query.pop();
                self.reconcile_query(permanent);
                self.request_reveal(ctx);
                continue;
            }
            let Some(action) = self.vm.action_for(&event) else {
                continue;
            };
            let navigation = matches!(
                action,
                keymap::Action::NavigateUp
                    | keymap::Action::NavigateDown
                    | keymap::Action::PageUp
                    | keymap::Action::PageDown
                    | keymap::Action::Home
                    | keymap::Action::End
            );
            let query_changed = matches!(action, keymap::Action::TypeChar(_));
            let effect = if permanent {
                self.permanent_action(action)
            } else {
                self.vm.clamp_selection();
                self.vm.on_action(action)
            };
            self.apply_effect(effect);
            if query_changed {
                self.reconcile_query(permanent);
            }
            if navigation || query_changed || matches!(action, keymap::Action::GroupTab(_)) {
                self.request_reveal(ctx);
            }
        }
    }

    fn reconcile_query(&mut self, permanent: bool) {
        if permanent {
            if let Ok(mut config) = self.config.lock() {
                config.perm_vm.selected = 0;
            }
        } else {
            self.vm.selected = 0;
            self.reset_compact(false);
        }
    }

    fn permanent_action(&mut self, action: keymap::Action) -> popup::VmEffect {
        use keymap::Action;
        let Ok(mut config) = self.config.lock() else {
            return popup::VmEffect::None;
        };
        let vm = &mut config.perm_vm;
        let query = &self.vm.query;
        vm.clamp_for(query);
        match action {
            Action::NavigateUp => vm.move_selection(-1, query),
            Action::NavigateDown => vm.move_selection(1, query),
            Action::PageUp => vm.move_selection(-10, query),
            Action::PageDown => vm.move_selection(10, query),
            Action::Home => vm.home(query),
            Action::End => vm.end(query),
            Action::Paste | Action::PasteInto => {
                return vm
                    .selected_id(query)
                    .map_or(popup::VmEffect::None, popup::VmEffect::ExecPermanent)
            }
            Action::Dismiss => return popup::VmEffect::Dismiss,
            Action::Refresh => return popup::VmEffect::Refresh,
            Action::GroupTab(n) => return popup::VmEffect::GroupTab(n),
            Action::TypeChar(c) => self.vm.query.push(c),
            Action::Remove | Action::Pin | Action::Edit | Action::None => {}
        }
        popup::VmEffect::None
    }

    /// Paint the footer and report whether Settings was activated.
    fn render_footer(&self, ui: &mut egui::Ui) -> bool {
        if let Some(error) = self.shell.error() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if !self.vm.banner().is_empty() {
            ui.label(self.vm.banner());
        }
        ui.label(self.vm.status_line());
        ui.button(strings::text("config.open_config")).clicked()
    }

    fn thumbnail_slot(
        &mut self,
        ui: &mut egui::Ui,
        clip: &clipmonger_core::model::Clip,
        demand: &mut Vec<crate::thumbnail_worker::Key>,
    ) {
        if let Some(thumbs) = &mut self.thumbnails {
            let hash = clip
                .formats
                .iter()
                .any(|mime| mime == "image/png")
                .then_some(clip.content_hash.as_str());
            thumbs.slot(ui, clip.id, hash, demand);
        } else {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            super::thumbnail_ui::placeholder(ui, rect);
        }
    }

    fn compact_thumbnail_slot(
        &mut self,
        ui: &mut egui::Ui,
        row: &popup::CompactRow,
        demand: &mut Vec<crate::thumbnail_worker::Key>,
    ) {
        if let Some(thumbs) = &mut self.thumbnails {
            thumbs.slot(ui, row.id, row.thumbnail_hash.as_deref(), demand);
        } else {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            super::thumbnail_ui::placeholder(ui, rect);
        }
    }

    /// Render the popup window contents.
    #[allow(clippy::too_many_lines)]
    pub(super) fn render(&mut self, ui: &mut egui::Ui) {
        let events = ui.input(|input| input.events.clone());
        let mut query = self.vm.query.clone();
        let mut clicked_row: Option<usize> = Option::None;
        let mut clicked_perm_row: Option<usize> = Option::None;
        let mut perm_tab_click: Option<Option<usize>> = None;
        let mut pin_row: Option<usize> = Option::None;
        let mut delete_row: Option<usize> = Option::None;
        let mut search_focused = false;
        let mut first_page = false;
        let mut next_page = false;
        let mut thumbnail_demand = Vec::new();
        // List content height minus the height the list may use.
        let mut list_overflow: Option<f32> = None;
        if let Some(thumbs) = &mut self.thumbnails {
            thumbs.begin_frame();
        }
        if let Ok(mut config) = self.config.lock() {
            if config.perm_source.is_some() {
                if !self.shell.desired().settings_open {
                    if query != self.last_query {
                        config.perm_vm.selected = 0;
                    }
                    config.perm_vm.clamp_for(&query);
                }
                config.perm_source = (!config.perm_vm.group_names().is_empty())
                    .then_some(config.perm_vm.active_group);
            } else {
                self.vm.clamp_selection();
            }
        }
        let current = self.compute_reveal_key();
        if current != self.last_reveal || query != self.last_query || self.pending_reveal.is_some()
        {
            self.pending_reveal = current.clone();
        }
        self.last_reveal = current;
        let pending = self.pending_reveal.take();

        let mut open_settings = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let full_rect = ui.max_rect();
            let footer_height = {
                let mut measure = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("popup_footer_measure")
                        .sizing_pass()
                        .invisible(),
                );
                self.render_footer(&mut measure);
                measure.min_size().y
            };
            // CentralPanel's min rect already spans the window, so `set_max_height`
            // cannot shrink it; the scroll areas stop at the footer instead.
            let content_bottom = full_rect.bottom() - footer_height - ui.spacing().item_spacing.y;
            // Resolve pointer focus before TextEdit consumes a batched clipboard event.
            let search_id = ui.make_persistent_id("popup_search");
            if let Some(response) = ui.ctx().read_response(search_id) {
                if response.clicked() {
                    response.request_focus();
                } else if ui.input(|input| input.pointer.any_pressed())
                    && !response.contains_pointer()
                {
                    response.surrender_focus();
                }
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .id(search_id)
                    .hint_text(strings::text("popup.search_hint")),
            );
            if std::mem::take(&mut self.focus_search) {
                field.request_focus();
            }
            search_focused = field.has_focus();
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
                let rows: Vec<(usize, String, bool, uuid::Uuid)> = self
                    .config
                    .lock()
                    .map(|config| {
                        let visible = config.perm_vm.visible(&query);
                        visible
                            .iter()
                            .enumerate()
                            .filter_map(|(row, &index)| {
                                let clip = config.perm_vm.permanents.get(index)?;
                                let name = clip.name.clone();
                                let selected = config.perm_vm.selected == row;
                                Some((row, name, selected, clip.id))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if rows.is_empty() {
                    ui.label(strings::text("popup.empty"));
                }
                let list_height = (content_bottom - ui.cursor().top()).max(0.0);
                let permanents_area = egui::ScrollArea::vertical()
                    .animated(false)
                    .min_scrolled_height(0.0)
                    .max_height(list_height)
                    .id_salt((
                        "popup_permanents",
                        group_names.get(perm_source.unwrap_or(0)),
                    ))
                    .show(ui, |ui| {
                        for (row, name, selected, id) in &rows {
                            let response = ui.selectable_label(*selected, name);
                            if *selected
                                && pending.as_ref().is_some_and(
                                    |(source, selected, selected_id)| {
                                        source.as_ref() == group_names.get(perm_source.unwrap_or(0))
                                            && *selected == *row
                                            && selected_id == id
                                    },
                                )
                            {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                clicked_perm_row = Some(*row);
                            }
                        }
                    });
                list_overflow = Some(permanents_area.content_size.y - list_height);
            } else {
                let count = self.vm.visible().len();
                if self.compact.uri_search_incomplete {
                    ui.label(strings::text("popup.file_search_incomplete"));
                }
                if self.compact.loading {
                    ui.label(strings::text("popup.loading"));
                } else if self.compact.error {
                    let message = match self.compact.error_code.as_deref() {
                        Some("not_ready") => "popup.history_preparing",
                        Some("metadata_too_large") => "popup.history_metadata",
                        _ => "popup.history_error",
                    };
                    ui.label(strings::text(message));
                } else if count == 0 {
                    ui.label(strings::text("popup.empty"));
                }
                let history_end = self.compact.done && count > 0;
                ui.horizontal(|ui| {
                    first_page = ui
                        .add_enabled(
                            !self.compact.on_first_page(),
                            egui::Button::new(strings::text("popup.first_page")),
                        )
                        .clicked();
                    next_page = ui
                        .add_enabled(
                            !self.compact.loading && !self.compact.done && !self.compact.error,
                            egui::Button::new(strings::text("popup.next_page")),
                        )
                        .clicked();
                });
                #[cfg(test)]
                let mut history_clip = None;
                let list_height = (content_bottom - ui.cursor().top()).max(0.0);
                let history_area = egui::ScrollArea::vertical()
                    .animated(false)
                    .min_scrolled_height(0.0)
                    .max_height(list_height)
                    .id_salt("popup_history")
                    .show(ui, |ui| {
                        #[cfg(test)]
                        {
                            history_clip = Some(ui.clip_rect());
                        }
                        for row in 0..count {
                            let compact = self
                                .vm
                                .compact_rows()
                                .and_then(|rows| rows.get(row))
                                .cloned();
                            let legacy = self.vm.clip_at(row).cloned();
                            let Some((clip_id, pinned, preview)) = compact
                                .as_ref()
                                .map(|row| (row.id, row.pinned, row.preview.clone()))
                                .or_else(|| {
                                    legacy
                                        .as_ref()
                                        .map(|clip| (clip.id, clip.pinned, clip.preview.clone()))
                                })
                            else {
                                continue;
                            };
                            let selected = self.vm.selected == row;
                            let truncated = popup::display_preview(
                                &preview,
                                self.preview_width,
                                self.preview_lines,
                            );
                            // Register the row as clickable when its child UI is created.
                            // Adding click sense afterward clears focus on the next pass.
                            let row_rect = egui::Rect::from_min_size(
                                ui.next_widget_position(),
                                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                            );
                            let row_ui = ui.scope_builder(
                                egui::UiBuilder::new()
                                    .max_rect(row_rect)
                                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                                    .sense(egui::Sense::click()),
                                |ui| {
                                    if let Some(row) = &compact {
                                        if row.kind == clipmonger_core::store::CompactKind::Image {
                                            self.compact_thumbnail_slot(
                                                ui,
                                                row,
                                                &mut thumbnail_demand,
                                            );
                                        }
                                    } else if let Some(clip) = &legacy {
                                        if popup::preview_kind(clip) == "popup.kind_image" {
                                            self.thumbnail_slot(ui, clip, &mut thumbnail_demand);
                                        }
                                    }
                                    ui.selectable_label(selected, &truncated)
                                },
                            );
                            let label = row_ui.inner;
                            let response = row_ui.response;
                            if selected
                                && pending.as_ref().is_some_and(
                                    |(source, selected, selected_id)| {
                                        source.is_none()
                                            && *selected == row
                                            && *selected_id == clip_id
                                    },
                                )
                            {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() || label.clicked() {
                                clicked_row = Some(row);
                            }
                            response.container_context_menu(|ui| {
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
                        if history_end {
                            ui.weak(strings::text("popup.history_end"));
                        }
                    });
                list_overflow = Some(history_area.content_size.y - list_height);
                #[cfg(test)]
                if let Some(thumbs) = &mut self.thumbnails {
                    thumbs.history_area = Some((
                        history_area
                            .inner_rect
                            .intersect(history_clip.expect("history clip")),
                        history_area.state.clamped_offset(),
                    ));
                }
            }
            let mut footer = ui.new_child(egui::UiBuilder::new().id_salt("popup_footer").max_rect(
                egui::Rect::from_min_max(
                    egui::pos2(full_rect.left(), full_rect.bottom() - footer_height),
                    full_rect.max,
                ),
            ));
            open_settings = self.render_footer(&mut footer);
            ui.expand_to_include_rect(footer.min_rect());
        });
        if let Some(thumbs) = &mut self.thumbnails {
            thumbs.update(ui.ctx(), thumbnail_demand);
        }
        // A filtered list says nothing about the size the clips need.
        if let (Some(overflow), true) = (list_overflow, query.is_empty()) {
            let font = egui::TextStyle::Body.resolve(ui.style());
            let glyph = ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'n'));
            #[allow(clippy::cast_precision_loss)]
            let width = self.preview_width as f32 * glyph + FIT_MARGIN;
            let height = ui.ctx().viewport_rect().height() + overflow;
            self.popup_fit = Some((width, height));
            self.grow_popup(ui.ctx());
        }
        if first_page {
            self.reset_compact(false);
        }
        if next_page {
            self.compact.next_page();
            if let Some(thumbs) = &mut self.thumbnails {
                thumbs.revoke_visibility();
            }
            self.vm.set_compact_rows(Vec::new());
            if let Some(cmd) = self.compact.command() {
                self.send(cmd);
            }
        }
        if open_settings {
            self.open_settings(None);
        }
        if query != self.vm.query {
            self.vm.query.clone_from(&query);
            let permanent = self.config.lock().is_ok_and(|c| c.perm_source.is_some());
            self.reconcile_query(permanent);
            self.request_reveal(ui.ctx());
        }
        if let Some(tab) = perm_tab_click {
            match tab {
                None => {
                    if let Ok(mut config) = self.config.lock() {
                        config.perm_source = None;
                    }
                    if !self.compact.query_matches(&self.vm.query) {
                        self.reset_compact(false);
                    }
                    self.request_reveal(ui.ctx());
                }
                Some(index) => {
                    if let Ok(mut config) = self.config.lock() {
                        config.perm_vm.select_group(index);
                        config.perm_source = Some(config.perm_vm.active_group);
                        let q = self.vm.query.clone();
                        config.perm_vm.clamp_for(&q);
                    }
                    self.request_reveal(ui.ctx());
                }
            }
        }
        if let Some(row) = clicked_perm_row {
            let q = self.vm.query.clone();
            let activate = self.config.lock().map_or(None, |mut config| {
                if row == config.perm_vm.selected {
                    config.perm_vm.selected_id(&q)
                } else {
                    config.perm_vm.selected = row;
                    None
                }
            });
            if let Some(id) = activate {
                self.apply_effect(popup::VmEffect::ExecPermanent(id));
            } else {
                self.request_reveal(ui.ctx());
            }
        }
        if let Some(row) = pin_row {
            if let Some(id) = self
                .vm
                .compact_rows()
                .and_then(|rows| rows.get(row))
                .map(|r| r.id)
                .or_else(|| self.vm.clip_at(row).map(|clip| clip.id))
            {
                let pinned = !self.vm.is_pinned(id);
                self.send(ipc_bridge::UiCmd::PinClip(id, pinned));
            }
        }
        if let Some(row) = delete_row {
            if let Some(id) = self
                .vm
                .compact_rows()
                .and_then(|rows| rows.get(row))
                .map(|r| r.id)
                .or_else(|| self.vm.clip_at(row).map(|clip| clip.id))
            {
                self.send(ipc_bridge::UiCmd::DeleteClip(id));
            }
        }
        if let Some(row) = clicked_row {
            if row == self.vm.selected {
                let effect = self.vm.on_key(keymap::KeyEvent {
                    key: "Enter".to_string(),
                    ctrl: false,
                    alt: false,
                    shift: false,
                    super_key: false,
                });
                self.apply_effect(effect);
            } else {
                self.vm.selected = row;
                self.request_reveal(ui.ctx());
            }
        }
        self.handle_keys(ui.ctx(), search_focused, &events);
        self.last_query.clone_from(&self.vm.query);
    }
}
