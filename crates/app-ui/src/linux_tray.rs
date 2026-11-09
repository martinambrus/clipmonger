//! Linux tray menu model and ksni service.

#[cfg(target_os = "linux")]
use super::push::Wake;
use super::strings;
use super::tray_logic::{icon_sizes, icon_state, menu_entries, MenuEntry, TrayAction, TrayInputs};

/// One clickable Linux tray menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxMenuItem {
    /// strings-module id.
    pub id: &'static str,
    /// Resolved label.
    pub label: String,
    /// Shell action on click.
    pub action: TrayAction,
}

/// One Linux tray menu row derived from `menu_entries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxMenuRow {
    /// Clickable item.
    Item(LinuxMenuItem),
    /// Visual separator.
    Separator,
}

/// Map `menu_entries` output to Linux tray rows with resolved labels.
#[must_use]
pub fn linux_menu(inputs: &TrayInputs) -> Vec<LinuxMenuRow> {
    menu_entries(inputs)
        .into_iter()
        .map(|entry| match entry {
            MenuEntry::Separator => LinuxMenuRow::Separator,
            MenuEntry::Item(id) => LinuxMenuRow::Item(LinuxMenuItem {
                id,
                label: strings::text(id).to_string(),
                action: action_for_id(id),
            }),
        })
        .collect()
}

/// Shell action for one menu string id.
#[must_use]
pub fn action_for_id(id: &str) -> TrayAction {
    match id {
        "tray.show" => TrayAction::OpenPopup,
        "tray.pause" | "tray.resume" => TrayAction::TogglePause,
        "tray.pause_1h" => TrayAction::PauseFor1h,
        "tray.config" => TrayAction::OpenConfig,
        "tray.about" => TrayAction::About,
        _ => TrayAction::Quit,
    }
}

/// The tray icon for `inputs` at every size, as `(edge, ARGB32 bytes)`, so the
/// panel can pick the one that fits.
#[must_use]
pub fn icon_pixmaps(inputs: &TrayInputs) -> Vec<(i32, Vec<u8>)> {
    icon_sizes(icon_state(inputs))
        .iter()
        .map(|(edge, rgba)| {
            let argb = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|[red, green, blue, alpha]| [*alpha, *red, *green, *blue])
                .collect();
            (i32::try_from(*edge).unwrap_or(i32::MAX), argb)
        })
        .collect()
}

/// ksni tray carrying a snapshot of the inputs at creation.
#[cfg(target_os = "linux")]
struct KsniTray {
    tx: std::sync::mpsc::Sender<TrayAction>,
    inputs: TrayInputs,
    wake: Wake,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for KsniTray {
    fn id(&self) -> String {
        "clipmonger".to_string()
    }

    fn title(&self) -> String {
        strings::text("tray.tooltip").to_string()
    }

    /// No tray host yet (or it went away): log it and keep waiting, so
    /// the icon appears once a panel or bar starts.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("tray: no tray host yet ({reason:?}); waiting for one");
        let _ = clipmonger_core::runtime_log::global_emit(
            clipmonger_core::runtime_log::Role::Ui,
            clipmonger_core::runtime_log::Severity::Warn,
            "tray.unavailable",
            &[("class", "tray_waiting"), ("origin", "linux")],
        );
        true
    }

    fn status(&self) -> ksni::Status {
        if !self.inputs.attached || self.inputs.degraded {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon_pixmaps(&self.inputs)
            .into_iter()
            .map(|(edge, data)| ksni::Icon {
                width: edge,
                height: edge,
                data,
            })
            .collect()
    }

    /// Panels that honour `NeedsAttention` draw this one instead.
    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon_pixmap()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: strings::text("tray.tooltip").to_string(),
            description: if self.inputs.paused {
                strings::text("tray.paused_tip").to_string()
            } else {
                String::new()
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayAction::Toggle);
        (self.wake)();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        linux_menu(&self.inputs)
            .into_iter()
            .map(|row| match row {
                LinuxMenuRow::Separator => ksni::MenuItem::Separator,
                LinuxMenuRow::Item(item) => {
                    let tx = self.tx.clone();
                    let wake = self.wake.clone();
                    let action = item.action.clone();
                    ksni::MenuItem::Standard(StandardItem {
                        label: item.label,
                        activate: Box::new(move |_tray: &mut KsniTray| {
                            let _ = tx.send(action.clone());
                            wake();
                        }),
                        ..Default::default()
                    })
                }
            })
            .collect()
    }
}

#[cfg(target_os = "linux")]
type KsniHandle = ksni::blocking::Handle<KsniTray>;

/// Linux tray service handle: clicks arrive on the channel.
#[cfg(target_os = "linux")]
pub struct LinuxTray {
    rx: std::sync::mpsc::Receiver<TrayAction>,
    ready: std::sync::mpsc::Receiver<Option<KsniHandle>>,
    handle: Option<KsniHandle>,
    degraded_reported: bool,
    /// Inputs the tray shows, and the latest the app reported.
    shown: TrayInputs,
    wanted: TrayInputs,
}

#[cfg(target_os = "linux")]
impl LinuxTray {
    /// Spawn the ksni service on a dedicated thread; never blocks the caller.
    /// `wake` runs after every queued click, so that a host asleep in its event
    /// loop calls [`LinuxTray::poll`].
    #[must_use]
    pub fn spawn(inputs: &TrayInputs, wake: Wake) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let snapshot = *inputs;
        std::thread::spawn(move || {
            use ksni::blocking::TrayMethods as _;
            let tray = KsniTray {
                tx,
                inputs: snapshot,
                wake,
            };
            // A tray host (waybar, a panel) may start after us at login:
            // keep the item and register once a watcher appears.
            let _ = ready_tx.send(tray.assume_sni_available(true).spawn().ok());
        });
        Self {
            rx,
            ready,
            handle: None,
            degraded_reported: false,
            shown: snapshot,
            wanted: snapshot,
        }
    }

    /// Show `inputs` (pause/resume label, paused tint, tooltip); applied
    /// once the service is up, and only when they changed.
    pub fn show(&mut self, inputs: TrayInputs) {
        self.wanted = inputs;
        if self.wanted == self.shown {
            return;
        }
        if let Some(handle) = &self.handle {
            let wanted = self.wanted;
            if handle.update(|tray| tray.inputs = wanted).is_some() {
                self.shown = wanted;
            }
        }
    }

    /// Settle creation and drain clicked actions.
    pub fn poll(&mut self) -> Vec<TrayAction> {
        if self.handle.is_none() && !self.degraded_reported {
            match self.ready.try_recv() {
                Ok(Some(handle)) => {
                    self.handle = Some(handle);
                    let wanted = self.wanted;
                    self.show(wanted);
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.degraded_reported = true;
                    eprintln!(
                        "tray unavailable on Linux in this build: degraded, window still opens"
                    );
                    let _ = clipmonger_core::runtime_log::global_emit(
                        clipmonger_core::runtime_log::Role::Ui,
                        clipmonger_core::runtime_log::Severity::Warn,
                        "tray.unavailable",
                        &[("class", "tray_unavailable"), ("origin", "linux")],
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut out = Vec::new();
        while let Ok(action) = self.rx.try_recv() {
            out.push(action);
        }
        out
    }
}
