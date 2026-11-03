//! Shared X session plumbing (unix only): connections, atoms, synchronous
//! requestor reads (including INCR), and owner-side selection serving.
//!
//! All helpers take `&RustConnection` and never spawn threads; callers own
//! the threading policy (the monitor serves from its worker thread, the
//! transfer reads over short-lived per-call connections).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use clipmonger_platform::types::{Error, Result};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xproto::{self, Atom, AtomEnum, EventMask, PropMode, Window, WindowClass};
use x11rb14::protocol::Event;
use x11rb14::rust_connection::RustConnection;

use crate::mime;

/// Bounded budget for one selection transfer (A6: ~5 s read deadline).
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
/// Sleep backing a bounded poll only.
const POLL_STEP: Duration = Duration::from_millis(5);
/// Scratch property name for requestor conversions.
const TMP_PROP: &[u8] = b"CLIPMONGER_TMP";

/// Bounded FIFO that preserves monitor events consumed during a synchronous
/// conversion so the outer pump can handle them exactly once, in order.
///
/// Ownership:
/// - `Worker` owns one `DeferredQueue` that persists across all its
///   `convert_read`/`read_targets` calls (direct + INCR). It is drained
///   before every outer poll of the X connection.
/// - `X11Transfer` per-call reads own a local ephemeral queue that is
///   discarded after the call; they do not own manager duties (no replay
///   to the monitor). This avoids background threads or cross-lock replay.
///
/// Normal flow: `convert_read`/`incr_receive` push unrelated
/// event (`ClientMessage` `SAVE_TARGETS`, Xfixes ownership changes,
/// `SelectionClear`, `PropertyNotify` DELETE, etc.) via `try_push`; the
/// matching `SelectionNotify` (or current INCR `PropertyNotify` `NEW_VALUE`) is
/// consumed. `SelectionRequest` and active owner-side INCR acknowledgements
/// are served inline to avoid deadlock; scratch-property events are consumed.
///
/// INCR flow: `incr_receive` defers identically; its matching
/// `PropertyNotify` `NEW_VALUE` for the current chunk is consumed, others are
/// deferred.
///
/// Error/overflow: `try_push` fails closed when `len == capacity`; the
/// queue becomes sticky-terminal and every subsequent `check_terminal` and
/// conversion returns `Error::Unavailable("x11 monitor disconnected: ...")`.
/// Poll errors (`poll_for_event` Err) also set the sticky terminal and
/// return the same Unavailable. Normal owner errors (Denied/Timeout/
/// TooLarge/no-selection-owner) remain non-terminal and do not set the
/// terminal flag; callers may swallow them without disconnecting.
///
/// The bound is fixed and small; monitor callers that previously used
/// `.ok()`/`unwrap_or_default` must observe the terminal via
/// `check_terminal` after each conversion, otherwise `pump_events` will
/// surface the terminal on its next drain and fail the worker.
pub(crate) struct DeferredQueue {
    events: std::collections::VecDeque<Event>,
    terminal: Option<String>,
    capacity: usize,
}

impl DeferredQueue {
    pub(crate) const DEFAULT_CAPACITY: usize = 32;

    pub(crate) fn new() -> Self {
        Self {
            events: std::collections::VecDeque::new(),
            terminal: None,
            capacity: Self::DEFAULT_CAPACITY,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            events: std::collections::VecDeque::new(),
            terminal: None,
            capacity,
        }
    }

    pub(crate) fn try_push(&mut self, event: Event) -> Result<()> {
        if let Some(msg) = &self.terminal {
            return Err(Error::Unavailable(format!(
                "x11 monitor disconnected: {msg}"
            )));
        }
        if self.events.len() >= self.capacity {
            self.terminal = Some("deferred queue overflow".to_string());
            return Err(Error::Unavailable(
                "x11 monitor disconnected: deferred queue overflow".to_string(),
            ));
        }
        self.events.push_back(event);
        Ok(())
    }

    pub(crate) fn pop_front(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    #[cfg(test)]
    pub(crate) fn has_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    pub(crate) fn check_terminal(&self) -> Result<()> {
        if let Some(msg) = self.terminal_message() {
            return Err(Error::Unavailable(format!(
                "x11 monitor disconnected: {msg}"
            )));
        }
        Ok(())
    }

    pub(crate) fn set_terminal(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        if self.terminal.is_none() {
            self.terminal = Some(m);
        }
    }

    pub(crate) fn terminal_message(&self) -> Option<&str> {
        self.terminal.as_deref()
    }
}

#[derive(Clone, Copy)]
enum ReceivePhase {
    Direct { selection: Atom, target: Atom },
    Incr,
}

enum Incoming {
    Selection(xproto::SelectionNotifyEvent),
    Chunk,
    Served,
    Deferred,
    Housekeeping,
}

#[derive(Clone, Copy)]
pub(crate) enum OwnerEvent<'a> {
    Request(&'a xproto::SelectionRequestEvent),
    Delete(&'a xproto::PropertyNotifyEvent),
}

/// Classify exactly the events consumed by the synchronous receive loops.
/// Only notifications for our own scratch property are housekeeping; deletes
/// for another window/property remain available to the owner-side pump.
fn receive_event(
    event: Event,
    requestor: Window,
    property: Atom,
    phase: ReceivePhase,
    serve: &mut dyn FnMut(OwnerEvent<'_>) -> bool,
    deferred: &mut DeferredQueue,
) -> Result<Incoming> {
    deferred.check_terminal()?;
    match event {
        Event::SelectionNotify(notify)
            if matches!(phase, ReceivePhase::Direct { selection, target }
                if notify.requestor == requestor && notify.selection == selection
                    && notify.target == target && (notify.property == property || notify.property == 0)) =>
        {
            Ok(Incoming::Selection(notify))
        }
        Event::PropertyNotify(event)
            if event.state == xproto::Property::DELETE && serve(OwnerEvent::Delete(&event)) =>
        {
            Ok(Incoming::Served)
        }
        Event::PropertyNotify(event) if event.window == requestor && event.atom == property => {
            if matches!(phase, ReceivePhase::Incr) && event.state == xproto::Property::NEW_VALUE {
                Ok(Incoming::Chunk)
            } else {
                Ok(Incoming::Housekeeping)
            }
        }
        Event::SelectionRequest(req) => {
            serve(OwnerEvent::Request(&req));
            Ok(Incoming::Served)
        }
        other => {
            deferred.try_push(other)?;
            Ok(Incoming::Deferred)
        }
    }
}

/// Connect to `display` (or `$DISPLAY` when `None`).
pub(crate) fn connect(display: Option<&str>) -> Result<(RustConnection, usize)> {
    let (conn, screen) = x11rb14::connect(display)
        .map_err(|e| Error::Unavailable(format!("x11 connect failed: {e}")))?;
    Ok((conn, screen))
}

/// `$DISPLAY` (or the explicit override) for per-call connections.
pub(crate) fn display_name(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("DISPLAY").ok())
}

/// Create a small input-only window on `screen`'s root.
pub(crate) fn create_window(conn: &RustConnection, screen: usize) -> Result<Window> {
    let root = conn.setup().roots[screen].root;
    let win: Window = conn
        .generate_id()
        .map_err(|e| Error::Io(format!("x11 generate_id failed: {e}")))?;
    xproto::create_window(
        conn,
        x11rb14::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        10,
        10,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb14::COPY_FROM_PARENT,
        &xproto::CreateWindowAux::new(),
    )
    .map_err(|e| Error::Io(format!("x11 create_window failed: {e}")))?
    .check()
    .map_err(|e| Error::Io(format!("x11 create_window check failed: {e}")))?;
    conn.flush()
        .map_err(|e| Error::Io(format!("x11 flush failed: {e}")))?;
    Ok(win)
}

/// Current owner of `selection` (`0` = none).
pub(crate) fn selection_owner(conn: &RustConnection, selection: Atom) -> Window {
    xproto::get_selection_owner(conn, selection)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map_or(0, |reply| reply.owner)
}

/// Claim `selection` for `win` (best-effort timestamp).
pub(crate) fn claim_selection(conn: &RustConnection, win: Window, selection: Atom) -> Result<()> {
    xproto::set_selection_owner(conn, win, selection, x11rb14::CURRENT_TIME)
        .map_err(|e| Error::Io(format!("x11 set_selection_owner failed: {e}")))?
        .check()
        .map_err(|e| Error::Io(format!("x11 set_selection_owner check failed: {e}")))?;
    conn.flush()
        .map_err(|e| Error::Io(format!("x11 flush failed: {e}")))?;
    Ok(())
}

/// Whether `win` still exists (target-existence probe for paste).
pub(crate) fn window_alive(conn: &RustConnection, win: Window) -> bool {
    xproto::get_window_attributes(conn, win)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some()
}

/// Interned atoms used by the backend (one batch at connect time).
pub(crate) struct Atoms {
    pub clipboard: Atom,
    pub primary: Atom,
    pub manager: Atom,
    pub targets: Atom,
    pub timestamp: Atom,
    pub utf8: Atom,
    pub string_atom: Atom,
    pub text_atom: Atom,
    pub incr: Atom,
    pub integer: Atom,
    pub save_targets: Atom,
    pub html: Atom,
    pub rtf: Atom,
    pub png: Atom,
    pub uri_list: Atom,
    pub tmp_prop: Atom,
    pub net_active: Atom,
    pub net_wm_pid: Atom,
    pub net_wm_name: Atom,
    pub wm_class: Atom,
}

impl Atoms {
    /// Intern every atom the backend names (creating when absent).
    pub(crate) fn intern(conn: &RustConnection) -> Result<Self> {
        let atom = |name: &[u8]| {
            xproto::intern_atom(conn, false, name)
                .map_err(|e| Error::Io(format!("x11 intern_atom failed: {e}")))?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|e| Error::Io(format!("x11 intern_atom reply failed: {e}")))
        };
        Ok(Self {
            clipboard: atom(b"CLIPBOARD")?,
            primary: atom(b"PRIMARY")?,
            manager: atom(b"CLIPBOARD_MANAGER")?,
            targets: atom(b"TARGETS")?,
            timestamp: atom(b"TIMESTAMP")?,
            utf8: atom(b"UTF8_STRING")?,
            string_atom: atom(b"STRING")?,
            text_atom: atom(b"TEXT")?,
            incr: atom(b"INCR")?,
            integer: atom(b"INTEGER")?,
            save_targets: atom(b"SAVE_TARGETS")?,
            html: atom(b"text/html")?,
            rtf: atom(b"text/rtf")?,
            png: atom(b"image/png")?,
            uri_list: atom(b"text/uri-list")?,
            tmp_prop: atom(TMP_PROP)?,
            net_active: atom(b"_NET_ACTIVE_WINDOW")?,
            net_wm_pid: atom(b"_NET_WM_PID")?,
            net_wm_name: atom(b"_NET_WM_NAME")?,
            wm_class: atom(b"WM_CLASS")?,
        })
    }

    /// Resolve a canonical MIME (or `x-os/<atom>`) to a target atom,
    /// interning passthrough names on demand.
    pub(crate) fn atom_for_mime(&self, conn: &RustConnection, mime: &str) -> Result<Atom> {
        if let Some(mapped) = mime::canonical_to_x11(mime) {
            return Ok(self.named(mapped));
        }
        let name = mime.strip_prefix("x-os/").unwrap_or(mime);
        xproto::intern_atom(conn, false, name.as_bytes())
            .map_err(|e| Error::Io(format!("x11 intern target failed: {e}")))?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|e| Error::Io(format!("x11 intern target reply failed: {e}")))
    }

    /// Well-known target name to atom (unknown names yield `0`).
    fn named(&self, name: &str) -> Atom {
        match name {
            "TARGETS" => self.targets,
            "TIMESTAMP" => self.timestamp,
            "UTF8_STRING" => self.utf8,
            "STRING" => self.string_atom,
            "TEXT" => self.text_atom,
            "text/html" => self.html,
            "text/rtf" => self.rtf,
            "image/png" => self.png,
            "text/uri-list" => self.uri_list,
            _ => 0,
        }
    }
}

/// Reverse-map one atom to its name (best-effort).
pub(crate) fn atom_name(conn: &RustConnection, atom: Atom) -> Option<String> {
    xproto::get_atom_name(conn, atom)
        .ok()?
        .reply()
        .ok()
        .and_then(|reply| String::from_utf8(reply.name).ok())
}

/// Raw `SelectionNotify` bytes for `requestor` (trailing pad is zeroes).
fn notify_bytes(requestor: Window, selection: Atom, target: Atom, property: Atom) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = 31; // SelectionNotify
    bytes[8..12].copy_from_slice(&requestor.to_ne_bytes());
    bytes[12..16].copy_from_slice(&selection.to_ne_bytes());
    bytes[16..20].copy_from_slice(&target.to_ne_bytes());
    bytes[20..24].copy_from_slice(&property.to_ne_bytes());
    bytes
}

/// Answer one conversion with `SelectionNotify` (`property = 0` refuses).
pub(crate) fn send_selection_notify(
    conn: &RustConnection,
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
) -> Result<()> {
    xproto::send_event(
        conn,
        false,
        requestor,
        EventMask::NO_EVENT,
        notify_bytes(requestor, selection, target, property),
    )
    .map_err(|e| Error::Io(format!("x11 send notify failed: {e}")))?
    .check()
    .map_err(|e| Error::Io(format!("x11 send notify check failed: {e}")))?;
    conn.flush()
        .map_err(|e| Error::Io(format!("x11 flush failed: {e}")))?;
    Ok(())
}

/// Read one property value, deleting it (the INCR ACK).
fn take_property(
    conn: &RustConnection,
    win: Window,
    property: Atom,
    max_bytes: u64,
) -> Result<(Atom, Vec<u8>)> {
    // X11 counts 4-byte units. Request only enough for cap+1 (rounded
    // up), then inspect bytes_after before any concatenation or payload copy.
    let units = u32::try_from(max_bytes.saturating_add(4) / 4).unwrap_or(u32::MAX);
    let reply = xproto::get_property(conn, true, win, property, 0u32, 0, units)
        .map_err(|e| Error::Io(format!("x11 get_property failed: {e}")))?
        .reply()
        .map_err(|e| Error::Io(format!("x11 get_property reply failed: {e}")))?;
    if reply.bytes_after != 0 || reply.value.len() as u64 > max_bytes {
        let _ = xproto::delete_property(conn, win, property);
        return Err(Error::TooLarge("X11 property over cap".to_string()));
    }
    Ok((reply.type_, reply.value))
}

/// Convert `target` from `selection` into `property` on `requestor` and
/// return the bytes, following INCR to the zero-length chunk.
///
/// `serve` pumps owner-side duties for the caller's own window while the
/// transfer is in flight (the monitor serves its store from the same
/// connection; per-call transfer connections pass a no-op).
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_read(
    conn: &RustConnection,
    selection: Atom,
    target: Atom,
    requestor: Window,
    property: Atom,
    max_bytes: u64,
    timeout: Duration,
    serve: &mut dyn FnMut(OwnerEvent<'_>) -> bool,
    deferred: &mut DeferredQueue,
) -> Result<(Atom, Vec<u8>)> {
    if selection_owner(conn, selection) == 0 {
        return Err(Error::Unavailable("no selection owner".to_string()));
    }
    // INCR progress arrives as PropertyNotify on our window: select it or
    // chunk ACKs are never observed (the owner sees its own selection).
    let _ = xproto::change_window_attributes(
        conn,
        requestor,
        &xproto::ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .ok()
    .and_then(|cookie| cookie.check().ok());
    xproto::convert_selection(
        conn,
        requestor,
        selection,
        target,
        property,
        x11rb14::CURRENT_TIME,
    )
    .map_err(|e| Error::Io(format!("x11 convert_selection failed: {e}")))?
    .check()
    .map_err(|e| Error::Io(format!("x11 convert_selection check failed: {e}")))?;
    conn.flush()
        .map_err(|e| Error::Io(format!("x11 flush failed: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Timeout(
                "selection transfer timed out waiting for the owner".to_string(),
            ));
        }
        deferred.check_terminal()?;
        match conn.poll_for_event() {
            Ok(Some(event)) => {
                if let Incoming::Selection(notify) = receive_event(
                    event,
                    requestor,
                    property,
                    ReceivePhase::Direct { selection, target },
                    serve,
                    deferred,
                )? {
                    if notify.property == 0 {
                        return Err(Error::Denied("owner refused the conversion".to_string()));
                    }
                    let (actual, value) =
                        take_property(conn, requestor, property, max_bytes.max(4))?;
                    if actual == incr_atom(conn) {
                        return incr_receive(
                            conn, requestor, property, &value, max_bytes, deadline, serve, deferred,
                        );
                    }
                    if value.len() as u64 > max_bytes {
                        return Err(Error::TooLarge("X11 selection over cap".to_string()));
                    }
                    return Ok((actual, value));
                }
            }
            Ok(None) => std::thread::sleep(POLL_STEP),
            Err(e) => {
                deferred.set_terminal(format!("x11 event poll failed: {e}"));
                return Err(Error::Unavailable(format!(
                    "x11 monitor disconnected: x11 event poll failed: {e}"
                )));
            }
        }
    }
}

/// The INCR atom of `conn` (interned; always present once used).
fn incr_atom(conn: &RustConnection) -> Atom {
    xproto::intern_atom(conn, false, b"INCR")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map_or(0, |reply| reply.atom)
}

/// Receive INCR chunks after the announce (`announce` holds the total
/// size); each chunk is deleted to ACK it.
#[expect(
    clippy::too_many_arguments,
    reason = "INCR receive needs the transfer bounds and owner service state"
)]
fn incr_receive(
    conn: &RustConnection,
    requestor: Window,
    property: Atom,
    announce: &[u8],
    max_bytes: u64,
    deadline: Instant,
    serve: &mut dyn FnMut(OwnerEvent<'_>) -> bool,
    deferred: &mut DeferredQueue,
) -> Result<(Atom, Vec<u8>)> {
    let mut size_bytes = [0u8; 4];
    let size_len = announce.len().min(4);
    size_bytes[..size_len].copy_from_slice(&announce[..size_len]);
    let total = u64::from(u32::from_ne_bytes(size_bytes));
    if total > max_bytes {
        let _ = xproto::delete_property(conn, requestor, property);
        return Err(Error::TooLarge(format!(
            "INCR payload of {total} bytes exceeds {max_bytes} bytes"
        )));
    }
    // ACK the announce so the owner starts sending chunks.
    let _ = xproto::delete_property(conn, requestor, property);
    let _ = conn.flush();
    let mut buf = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Timeout(
                "INCR transfer timed out before the final chunk".to_string(),
            ));
        }
        deferred.check_terminal()?;
        match conn.poll_for_event() {
            Ok(Some(event)) => {
                if let Incoming::Chunk = receive_event(
                    event,
                    requestor,
                    property,
                    ReceivePhase::Incr,
                    serve,
                    deferred,
                )? {
                    let (kind, chunk) = take_property(
                        conn,
                        requestor,
                        property,
                        max_bytes.saturating_sub(buf.len() as u64),
                    )?;
                    if chunk.is_empty() {
                        return Ok((kind, buf));
                    }
                    if (buf.len() as u64) + (chunk.len() as u64) > max_bytes {
                        return Err(Error::TooLarge(format!(
                            "INCR payload exceeds {max_bytes} bytes"
                        )));
                    }
                    buf.extend_from_slice(&chunk);
                }
            }
            Ok(None) => std::thread::sleep(POLL_STEP),
            Err(e) => {
                deferred.set_terminal(format!("x11 event poll failed: {e}"));
                return Err(Error::Unavailable(format!(
                    "x11 monitor disconnected: x11 event poll failed: {e}"
                )));
            }
        }
    }
}

/// Convert `TARGETS` from `selection` and return the offered atom list.
#[expect(
    clippy::too_many_arguments,
    reason = "TARGETS conversion needs the requestor and owner service state"
)]
pub(crate) fn read_targets(
    conn: &RustConnection,
    selection: Atom,
    targets_atom: Atom,
    requestor: Window,
    property: Atom,
    timeout: Duration,
    serve: &mut dyn FnMut(OwnerEvent<'_>) -> bool,
    deferred: &mut DeferredQueue,
) -> Result<Vec<Atom>> {
    let (actual, value) = convert_read(
        conn,
        selection,
        targets_atom,
        requestor,
        property,
        64 << 10,
        timeout,
        serve,
        deferred,
    )?;
    if actual != Atom::from(AtomEnum::ATOM) || !value.len().is_multiple_of(4) {
        return Ok(Vec::new());
    }
    let (chunks, _) = value.as_chunks::<4>();
    Ok(chunks
        .iter()
        .map(|chunk| Atom::from_ne_bytes(*chunk))
        .collect())
}

/// Server limit for one request, in bytes.
pub(crate) fn max_request_bytes(conn: &RustConnection) -> usize {
    (conn.setup().maximum_request_length as usize).saturating_mul(4)
}

/// Payloads currently served as the selection owner.
#[derive(Debug, Default)]
pub(crate) struct ServedStore {
    /// Canonical MIME payloads (`x-os/*` included verbatim).
    pub payloads: Vec<(String, Vec<u8>)>,
}

impl ServedStore {
    /// Look up the bytes served for target atom `name`.
    fn for_target(&self, name: &str) -> Option<(&str, &[u8])> {
        let canonical = mime::x11_to_canonical(name)?;
        self.payloads
            .iter()
            .find(|(mime, _)| *mime == canonical)
            .map(|(mime, bytes)| (mime.as_str(), bytes.as_slice()))
    }
}

/// One outstanding INCR send to a requestor.
#[derive(Debug)]
struct IncrSend {
    chunks: Vec<Vec<u8>>,
    next: usize,
    reply_type: Atom,
    selection: Atom,
    target: Atom,
}

/// Owner-side server state shared by the serving threads.
#[derive(Debug, Default)]
pub(crate) struct ServerState {
    /// Payloads served for our owned selections.
    pub store: ServedStore,
    /// INCR sends in flight, keyed by (requestor, property).
    pending: HashMap<(Window, Atom), IncrSend>,
    /// Last server timestamp observed (for `TIMESTAMP` + indicators).
    pub last_time: u32,
}

impl ServerState {
    pub(crate) fn new() -> Self {
        Self {
            store: ServedStore::default(),
            pending: HashMap::new(),
            last_time: 0,
        }
    }
}

/// Serve requests and acknowledgements that can progress without starting a
/// nested selection conversion. Other deletes remain ordered in the FIFO.
pub(crate) fn serve_owner_event(
    conn: &RustConnection,
    atoms: &Atoms,
    state: &mut ServerState,
    event: OwnerEvent<'_>,
) -> bool {
    match event {
        OwnerEvent::Request(req) => {
            serve_selection_request(conn, atoms, req, state);
            true
        }
        OwnerEvent::Delete(ev) if state.pending.contains_key(&(ev.window, ev.atom)) => {
            serve_property_delete(conn, ev.window, ev.atom, state);
            true
        }
        OwnerEvent::Delete(_) => false,
    }
}

/// Answer one `SelectionRequest` from `store` (direct or INCR-send).
#[allow(clippy::too_many_lines)]
pub(crate) fn serve_selection_request(
    conn: &RustConnection,
    atoms: &Atoms,
    req: &x11rb14::protocol::xproto::SelectionRequestEvent,
    state: &mut ServerState,
) {
    state.last_time = req.time;
    if req.time == 0 {
        state.last_time = 1;
    }
    let name = atom_name(conn, req.target).unwrap_or_default();
    if req.target == atoms.targets {
        let mimes: Vec<String> = state
            .store
            .payloads
            .iter()
            .map(|(mime, _)| mime.clone())
            .collect();
        let names = mime::targets_for(&mimes);
        let mut bytes = Vec::with_capacity(names.len() * 4);
        for target in &names {
            let atom = match target.as_str() {
                "TARGETS" => atoms.targets,
                "TIMESTAMP" => atoms.timestamp,
                _ => atoms.named(target),
            };
            let atom = if atom == 0 {
                xproto::intern_atom(conn, false, target.as_bytes())
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .map_or(0, |reply| reply.atom)
            } else {
                atom
            };
            bytes.extend_from_slice(&atom.to_ne_bytes());
        }
        let count = u32::try_from(bytes.len() / 4).unwrap_or(u32::MAX);
        let _ = xproto::change_property(
            conn,
            PropMode::REPLACE,
            req.requestor,
            req.property,
            AtomEnum::ATOM,
            32,
            count,
            &bytes,
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
        let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, req.property);
        return;
    }
    if req.target == atoms.timestamp {
        let stamp = state.last_time.to_ne_bytes();
        let _ = xproto::change_property(
            conn,
            PropMode::REPLACE,
            req.requestor,
            req.property,
            atoms.integer,
            32,
            1,
            &stamp,
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
        let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, req.property);
        return;
    }
    let Some((_mime, bytes)) = state.store.for_target(&name) else {
        let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, 0);
        return;
    };
    let reply_type = match name.as_str() {
        "UTF8_STRING" | "STRING" | "TEXT" => atoms.utf8,
        "text/html" => atoms.html,
        "text/rtf" => atoms.rtf,
        "image/png" => atoms.png,
        "text/uri-list" => atoms.uri_list,
        _ => xproto::intern_atom(conn, false, name.as_bytes())
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or(atoms.utf8, |reply| reply.atom),
    };
    if bytes.len() <= max_request_bytes(conn).max(1024) {
        let owned: Vec<u8> = bytes.to_vec();
        let count = u32::try_from(owned.len()).unwrap_or(u32::MAX);
        let _ = xproto::change_property(
            conn,
            PropMode::REPLACE,
            req.requestor,
            req.property,
            reply_type,
            8,
            count,
            &owned,
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
        let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, req.property);
        return;
    }
    // INCR send: announce, then feed chunks as the requestor deletes.
    let total = u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_ne_bytes();
    let chunks: Vec<Vec<u8>> = bytes
        .chunks(max_request_bytes(conn).max(1024))
        .map(<[u8]>::to_vec)
        .collect();
    let ok = xproto::change_property(
        conn,
        PropMode::REPLACE,
        req.requestor,
        req.property,
        atoms.incr,
        32,
        1,
        &total,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok())
    .is_some();
    if !ok {
        let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, 0);
        return;
    }
    state.pending.insert(
        (req.requestor, req.property),
        IncrSend {
            chunks,
            next: 0,
            reply_type,
            selection: req.selection,
            target: req.target,
        },
    );
    let _ = send_selection_notify(conn, req.requestor, req.selection, req.target, req.property);
}

/// Advance INCR sends when a requestor deletes our property.
pub(crate) fn serve_property_delete(
    conn: &RustConnection,
    requestor: Window,
    property: Atom,
    state: &mut ServerState,
) {
    let key = (requestor, property);
    let Some(mut send) = state.pending.remove(&key) else {
        return;
    };
    if send.next < send.chunks.len() {
        let chunk = send.chunks[send.next].clone();
        send.next += 1;
        let done = send.next == send.chunks.len();
        let count = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
        let _ = xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            send.reply_type,
            8,
            count,
            &chunk,
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
        if !done {
            state.pending.insert(key, send);
            return;
        }
        // Final zero-length chunk goes out once the last chunk is ACKed;
        // re-arm with an empty remainder.
        state.pending.insert(
            key,
            IncrSend {
                chunks: Vec::new(),
                next: 1,
                reply_type: send.reply_type,
                selection: send.selection,
                target: send.target,
            },
        );
    } else {
        let _ = xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            send.reply_type,
            8,
            0,
            &[],
        )
        .ok()
        .and_then(|cookie| cookie.check().ok());
    }
}

#[cfg(test)]
mod deferred_tests {
    use super::*;
    use x11rb14::protocol::xproto::{ClientMessageEvent, PropertyNotifyEvent, SelectionClearEvent};
    use x11rb14::protocol::Event;

    fn client_msg(save_targets: u32) -> Event {
        Event::ClientMessage(ClientMessageEvent {
            response_type: 33,
            sequence: 0,
            window: 1,
            type_: save_targets,
            format: 32,
            data: [0u32; 5].into(),
        })
    }

    fn selection_clear(manager: u32) -> Event {
        Event::SelectionClear(SelectionClearEvent {
            response_type: 0,
            sequence: 0,
            time: 0,
            selection: manager,
            owner: 2,
        })
    }

    fn property_delete(window: u32, atom: u32) -> Event {
        Event::PropertyNotify(PropertyNotifyEvent {
            response_type: 0,
            sequence: 0,
            window,
            atom,
            time: 0,
            state: x11rb14::protocol::xproto::Property::DELETE,
        })
    }

    fn notify(selection: u32, target: u32, property: u32) -> Event {
        Event::SelectionNotify(xproto::SelectionNotifyEvent {
            response_type: 31,
            sequence: 0,
            time: 0,
            requestor: 1,
            selection,
            target,
            property,
        })
    }

    fn request() -> Event {
        Event::SelectionRequest(xproto::SelectionRequestEvent {
            response_type: 30,
            sequence: 0,
            time: 0,
            owner: 1,
            requestor: 7,
            selection: 2,
            target: 3,
            property: 8,
        })
    }

    #[test]
    fn receive_classifier_preserves_unmatched_and_serves_inline() {
        let mut q = DeferredQueue::new();
        let mut requests = 0;
        let mut serve = |event: OwnerEvent<'_>| match event {
            OwnerEvent::Request(_) => {
                requests += 1;
                true
            }
            OwnerEvent::Delete(_) => false,
        };
        let phase = ReceivePhase::Direct {
            selection: 2,
            target: 3,
        };
        assert!(matches!(
            receive_event(request(), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Served
        ));
        assert!(matches!(
            receive_event(notify(2, 5, 4), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Deferred
        ));
        assert!(matches!(
            receive_event(client_msg(10), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Deferred
        ));
        assert!(matches!(
            receive_event(selection_clear(20), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Deferred
        ));
        let owner_change =
            Event::XfixesSelectionNotify(x11rb14::protocol::xfixes::SelectionNotifyEvent {
                response_type: 0,
                subtype: x11rb14::protocol::xfixes::SelectionEvent::SET_SELECTION_OWNER,
                sequence: 0,
                window: 1,
                owner: 12,
                selection: 2,
                timestamp: 0,
                selection_timestamp: 0,
            });
        assert!(matches!(
            receive_event(owner_change, 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Deferred
        ));
        assert!(matches!(
            receive_event(property_delete(7, 8), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Deferred
        ));
        assert!(matches!(
            receive_event(notify(2, 3, 4), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Selection(_)
        ));
        assert!(matches!(
            receive_event(notify(2, 3, 0), 1, 4, phase, &mut serve, &mut q).unwrap(),
            Incoming::Selection(_)
        ));
        assert!(matches!(q.pop_front(), Some(Event::SelectionNotify(_))));
        assert!(matches!(q.pop_front(), Some(Event::ClientMessage(_))));
        assert!(matches!(q.pop_front(), Some(Event::SelectionClear(_))));
        assert!(matches!(
            q.pop_front(),
            Some(Event::XfixesSelectionNotify(_))
        ));
        assert!(matches!(q.pop_front(), Some(Event::PropertyNotify(_))));
        drop(serve);
        assert_eq!(requests, 1);
    }

    #[test]
    fn receive_classifier_handles_many_incr_acknowledgements_and_overflow() {
        let mut q = DeferredQueue::new();
        let mut served_delete = 0;
        let mut serve = |event: OwnerEvent<'_>| match event {
            OwnerEvent::Request(_) => true,
            OwnerEvent::Delete(e) if e.window == 7 && e.atom == 8 => {
                served_delete += 1;
                true
            }
            OwnerEvent::Delete(_) => false,
        };
        for _ in 0..64 {
            assert!(matches!(
                receive_event(
                    property_delete(1, 4),
                    1,
                    4,
                    ReceivePhase::Incr,
                    &mut serve,
                    &mut q
                )
                .unwrap(),
                Incoming::Housekeeping
            ));
            let Event::PropertyNotify(mut new_value) = property_delete(1, 4) else {
                unreachable!();
            };
            new_value.state = xproto::Property::NEW_VALUE;
            assert!(matches!(
                receive_event(
                    Event::PropertyNotify(new_value),
                    1,
                    4,
                    ReceivePhase::Incr,
                    &mut serve,
                    &mut q
                )
                .unwrap(),
                Incoming::Chunk
            ));
        }
        assert!(q.is_empty());
        assert!(matches!(
            receive_event(
                property_delete(7, 8),
                1,
                4,
                ReceivePhase::Incr,
                &mut serve,
                &mut q
            )
            .unwrap(),
            Incoming::Served
        ));
        assert!(matches!(
            receive_event(
                notify(2, 3, 4),
                1,
                4,
                ReceivePhase::Incr,
                &mut serve,
                &mut q
            )
            .unwrap(),
            Incoming::Deferred
        ));
        for i in 0..31 {
            receive_event(client_msg(i), 1, 4, ReceivePhase::Incr, &mut serve, &mut q).unwrap();
        }
        assert!(receive_event(
            selection_clear(1),
            1,
            4,
            ReceivePhase::Incr,
            &mut serve,
            &mut q
        )
        .is_err());
        assert!(receive_event(request(), 1, 4, ReceivePhase::Incr, &mut serve, &mut q).is_err());
        assert!(q.check_terminal().is_err());
        drop(serve);
        assert_eq!(served_delete, 1);
    }

    #[test]
    fn deferred_queue_preserves_fifo_order() {
        let mut q = DeferredQueue::new();
        let message = client_msg(100);
        let clear = selection_clear(200);
        let deletion = property_delete(3, 400);
        q.try_push(message).unwrap();
        q.try_push(clear).unwrap();
        q.try_push(deletion).unwrap();
        assert_eq!(q.len(), 3);
        let first = q.pop_front().unwrap();
        assert!(matches!(first, Event::ClientMessage(m) if m.type_ == 100));
        let second = q.pop_front().unwrap();
        assert!(matches!(second, Event::SelectionClear(m) if m.selection == 200));
        let third = q.pop_front().unwrap();
        assert!(matches!(third, Event::PropertyNotify(m) if m.atom == 400));
        assert!(q.is_empty());
    }

    #[test]
    fn deferred_queue_overflow_is_terminal_and_sticky() {
        let mut q = DeferredQueue::with_capacity(2);
        q.try_push(client_msg(1)).unwrap();
        q.try_push(client_msg(2)).unwrap();
        let err = q.try_push(client_msg(3)).unwrap_err();
        assert!(matches!(err, Error::Unavailable(m) if m.contains("overflow")));
        assert!(q.has_terminal());
        assert!(q.check_terminal().is_err());
        let err2 = q.try_push(client_msg(4)).unwrap_err();
        assert!(matches!(err2, Error::Unavailable(m) if m.contains("overflow")));
        assert!(q.pop_front().is_some());
        assert!(q.has_terminal());
    }

    #[test]
    fn deferred_queue_poll_error_sets_terminal() {
        let mut q = DeferredQueue::new();
        q.set_terminal("x11 event poll failed: closed");
        assert!(q.has_terminal());
        assert!(q.check_terminal().is_err());
        let err = q.try_push(client_msg(9)).unwrap_err();
        assert!(matches!(err, Error::Unavailable(m) if m.contains("monitor disconnected")));
    }

    #[test]
    fn deferred_queue_direct_and_incr_events_share_same_queue_semantics() {
        let mut q = DeferredQueue::with_capacity(32);
        q.try_push(client_msg(10)).unwrap();
        q.try_push(selection_clear(20)).unwrap();
        q.try_push(property_delete(5, 30)).unwrap();
        assert_eq!(q.len(), 3);
        let mut seen = Vec::new();
        while let Some(ev) = q.pop_front() {
            match ev {
                Event::ClientMessage(m) => seen.push(("client", m.type_)),
                Event::SelectionClear(m) => seen.push(("clear", m.selection)),
                Event::PropertyNotify(m) => seen.push(("prop", m.atom)),
                _ => seen.push(("other", 0)),
            }
        }
        assert_eq!(seen, vec![("client", 10), ("clear", 20), ("prop", 30)]);
    }

    #[test]
    fn deferred_queue_terminal_propagates_through_convert_helpers() {
        let mut q = DeferredQueue::new();
        q.set_terminal("deferred queue overflow");
        assert!(q.check_terminal().is_err());
        let err = q.check_terminal().unwrap_err();
        assert!(matches!(err, Error::Unavailable(m) if m.contains("overflow")));
    }
}
