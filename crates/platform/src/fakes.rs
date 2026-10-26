//! Scripted fakes for every platform trait; deterministic poll, no sleeps.

use std::cell::{Ref, RefCell};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use crate::traits::{
    AutostartManager, ClipboardMonitor, ClipboardTransfer, ForegroundApp, HotkeyManager,
    PasteEngine, TrayController,
};
use crate::types::{
    Affinity, AppId, CaptureEvent, ClipRef, HotkeyId, PasteStrategy, PasteTarget, PayloadMap,
    Result, SubscribeKind, TrayEvent, TrayState,
};

/// Scripted monitor: each `poll` pops the next queued batch.
#[derive(Debug, Default)]
pub struct FakeMonitor {
    /// Queued event batches, one per `poll`.
    script: VecDeque<Vec<CaptureEvent>>,
    /// Last subscribed kinds.
    pub kinds: Vec<SubscribeKind>,
}

impl FakeMonitor {
    /// Build from a poll-by-poll script.
    #[must_use]
    pub fn new(script: Vec<Vec<CaptureEvent>>) -> Self {
        Self {
            script: script.into(),
            kinds: Vec::new(),
        }
    }
}

impl ClipboardMonitor for FakeMonitor {
    const AFFINITY: Affinity = Affinity::Worker;

    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()> {
        self.kinds = kinds.to_vec();
        Ok(())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Vec<CaptureEvent>> {
        Ok(self.script.pop_front().unwrap_or_default())
    }
}

/// Scripted transfer: canned formats/data, recorded writes, fixed indicator.
#[derive(Debug, Default)]
pub struct FakeTransfer {
    /// Advertised formats.
    formats: Vec<String>,
    /// Canned payload per MIME.
    data: HashMap<String, Vec<u8>>,
    /// Payloads received via `write`, in order.
    writes: RefCell<Vec<PayloadMap>>,
    /// Fixed unchanged-indicator.
    indicator: Option<String>,
}

impl FakeTransfer {
    /// Build with canned formats, data, and unchanged-indicator.
    #[must_use]
    pub fn new(
        formats: Vec<String>,
        data: HashMap<String, Vec<u8>>,
        indicator: Option<String>,
    ) -> Self {
        Self {
            formats,
            data,
            writes: RefCell::new(Vec::new()),
            indicator,
        }
    }

    /// Recorded `write` payloads, in order.
    #[must_use]
    pub fn writes(&self) -> Ref<'_, Vec<PayloadMap>> {
        self.writes.borrow()
    }
}

impl ClipboardTransfer for FakeTransfer {
    const AFFINITY: Affinity = Affinity::Worker;

    fn formats(&self) -> Result<Vec<String>> {
        Ok(self.formats.clone())
    }

    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut bytes = self.data.get(mime).cloned().unwrap_or_default();
        bytes.truncate(usize::try_from(max_bytes).unwrap_or(usize::MAX));
        Ok(bytes)
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        self.writes.borrow_mut().push(payload.clone());
        Ok(())
    }

    fn unchanged_indicator(&self) -> Option<String> {
        self.indicator.clone()
    }
}

/// Scripted hotkeys: each `poll` pops the next queued id batch.
#[derive(Debug, Default)]
pub struct FakeHotkeys {
    /// Queued id batches, one per `poll`.
    script: VecDeque<Vec<HotkeyId>>,
    /// Active registrations.
    pub registered: HashMap<HotkeyId, String>,
}

impl FakeHotkeys {
    /// Build from a poll-by-poll script.
    #[must_use]
    pub fn new(script: Vec<Vec<HotkeyId>>) -> Self {
        Self {
            script: script.into(),
            registered: HashMap::new(),
        }
    }
}

impl HotkeyManager for FakeHotkeys {
    const AFFINITY: Affinity = Affinity::MainThread;

    fn register(&mut self, id: HotkeyId, chord: &str) -> Result<()> {
        let parsed = crate::chord::parse(chord)?;
        self.registered.insert(id, parsed.to_string());
        Ok(())
    }

    fn unregister(&mut self, id: HotkeyId) -> Result<()> {
        self.registered.remove(&id);
        Ok(())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Vec<HotkeyId>> {
        Ok(self.script.pop_front().unwrap_or_default())
    }
}

/// Scripted paste engine: fixed target, recorded pastes with revalidation.
#[derive(Debug)]
pub struct FakePaste {
    /// Delivery strategy.
    strategy: PasteStrategy,
    /// Target returned by `capture_target`.
    target: PasteTarget,
    /// Recorded `(target, clip)` pastes.
    pastes: RefCell<Vec<(PasteTarget, ClipRef)>>,
    /// When false, the target reads as dead.
    pub alive: bool,
}

impl FakePaste {
    /// Build with a strategy and the target to return.
    #[must_use]
    pub fn new(strategy: PasteStrategy, target: PasteTarget) -> Self {
        Self {
            strategy,
            target,
            pastes: RefCell::new(Vec::new()),
            alive: true,
        }
    }

    /// Recorded pastes, in order.
    #[must_use]
    pub fn pastes(&self) -> Ref<'_, Vec<(PasteTarget, ClipRef)>> {
        self.pastes.borrow()
    }
}

impl PasteEngine for FakePaste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        self.strategy
    }

    fn capture_target(&self) -> Result<PasteTarget> {
        Ok(self.target.clone())
    }

    fn paste_to(&self, target: &PasteTarget, clip: &ClipRef) -> Result<()> {
        let current = self.alive.then(|| self.target.clone());
        crate::paste_policy::check_target(target, current.as_ref())?;
        self.pastes.borrow_mut().push((target.clone(), *clip));
        Ok(())
    }
}

/// Scripted tray: state/tooltip/menu setters, queued UI events.
#[derive(Debug, Default)]
pub struct FakeTray {
    /// Queued UI events, one batch per `poll_events`... drained fully.
    events: VecDeque<TrayEvent>,
    /// Last attention state.
    pub state: Option<TrayState>,
    /// Last tooltip.
    pub tooltip: Option<String>,
    /// Last menu.
    pub menu: Vec<String>,
}

impl FakeTray {
    /// Build with queued UI events.
    #[must_use]
    pub fn new(events: Vec<TrayEvent>) -> Self {
        Self {
            events: events.into(),
            state: None,
            tooltip: None,
            menu: Vec::new(),
        }
    }
}

impl TrayController for FakeTray {
    const AFFINITY: Affinity = Affinity::MainThread;

    fn set_state(&mut self, state: TrayState) {
        self.state = Some(state);
    }

    fn set_tooltip(&mut self, tooltip: &str) {
        self.tooltip = Some(tooltip.to_string());
    }

    fn set_menu(&mut self, items: &[String]) {
        self.menu = items.to_vec();
    }

    fn poll_events(&mut self) -> Vec<TrayEvent> {
        std::mem::take(&mut self.events).into_iter().collect()
    }
}

/// Scripted autostart flag.
#[derive(Debug, Default)]
pub struct FakeAutostart {
    /// Current flag.
    enabled: bool,
}

impl FakeAutostart {
    /// Build with an initial flag.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl AutostartManager for FakeAutostart {
    const AFFINITY: Affinity = Affinity::Worker;

    fn enable(&mut self) -> Result<()> {
        self.enabled = true;
        Ok(())
    }

    fn disable(&mut self) -> Result<()> {
        self.enabled = false;
        Ok(())
    }

    fn is_enabled(&self) -> Result<bool> {
        Ok(self.enabled)
    }
}

/// Scripted foreground app (v2 `AutoSwitch` hook).
#[derive(Debug, Default)]
pub struct FakeForeground {
    /// App returned by `active`.
    app: Option<AppId>,
}

impl FakeForeground {
    /// Build returning `app` (None = unknown foreground).
    #[must_use]
    pub fn new(app: Option<AppId>) -> Self {
        Self { app }
    }
}

impl ForegroundApp for FakeForeground {
    const AFFINITY: Affinity = Affinity::Worker;

    fn active(&self) -> Option<AppId> {
        self.app.clone()
    }
}
