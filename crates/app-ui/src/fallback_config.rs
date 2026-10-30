#![allow(unsafe_code)]
use crate::{config_model, ipc_bridge};
use clipmonger_ipc::client::Client;
use clipmonger_ipc::framing;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetStockObject, InvalidateRect, HBRUSH, HDC, PAINTSTRUCT,
    WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, SendMessageW,
    ShowWindow, TranslateMessage, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, EN_CHANGE, LBN_SELCHANGE, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, MSG,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_SIZE, WNDCLASSW,
    WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE, WS_VSCROLL,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, WM_ERASEBKGND, WM_PAINT, WM_TIMER};

const IDC_TAB: i32 = 201;
const IDC_FILTER: i32 = 202;
const IDC_ROWS: i32 = 203;
const IDC_VALUE: i32 = 204;
const IDC_APPLY: i32 = 205;
const IDC_REVERT: i32 = 206;
const IDC_STATUS: i32 = 207;
const IDC_SET: i32 = 208;
const TIMER_CFG: usize = 2;

struct CfgState {
    cmds: std::sync::mpsc::Sender<ipc_bridge::UiCmd>,
    events: std::sync::mpsc::Receiver<ipc_bridge::UiEvent>,
    doc: Option<config_model::ConfigDoc>,
    filter: String,
    tab: String,
    selected: Option<usize>,
    filtered: Vec<usize>,
    hwnd_tab: HWND,
    hwnd_filter: HWND,
    hwnd_rows: HWND,
    hwnd_value: HWND,
    hwnd_apply: HWND,
    hwnd_revert: HWND,
    hwnd_status: HWND,
    hwnd_set: HWND,
    last_applied: Option<Instant>,
    last_applied_key: Option<String>,
}

static mut G_CFG: Option<*mut CfgState> = None;
static mut G_CFG_EDIT_ORIG: isize = 0;
static mut G_CFG_EDIT_ORIG2: isize = 0;
unsafe extern "system" fn cfg_filter_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == 0x0100 || msg == 0x0102 {
        let vk = wparam.0 as i32;
        if vk == 0x41 || vk == 0x61 || wparam.0 == 1 {
            let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            if ctrl || wparam.0 == 1 {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    EM_SETSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(-1)),
                );
                return LRESULT(0);
            }
        }
    }
    windows::Win32::UI::WindowsAndMessaging::CallWindowProcW(
        std::mem::transmute(G_CFG_EDIT_ORIG),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}
unsafe extern "system" fn cfg_value_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == 0x0100 || msg == 0x0102 {
        let vk = wparam.0 as i32;
        if vk == 0x41 || vk == 0x61 || wparam.0 == 1 {
            let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            if ctrl || wparam.0 == 1 {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    EM_SETSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(-1)),
                );
                return LRESULT(0);
            }
        }
    }
    windows::Win32::UI::WindowsAndMessaging::CallWindowProcW(
        std::mem::transmute(G_CFG_EDIT_ORIG2),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

unsafe fn refresh_rows(state: &mut CfgState) {
    SendMessageW(
        state.hwnd_rows,
        LB_RESETCONTENT,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    let filtered = config_model::filter_rows(&state.filter);
    let mut visible = Vec::new();
    for idx in filtered {
        let row = config_model::ROWS[idx];
        if format!("{:?}", row.tab) != state.tab {
            continue;
        }
        if state.tab == "PermanentClips" && row.key == "permanents.groups" {
            continue;
        }
        visible.push(idx);
    }
    state.filtered = visible.clone();
    for idx in visible {
        let row = config_model::ROWS[idx];
        let val = state
            .doc
            .as_ref()
            .and_then(|d| d.get(row.key).or(d.staged(row.key)));
        let line = if let Some(v) = val {
            format!("{} = {}", row.key, v)
        } else {
            row.key.to_string()
        };
        let wide: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
        SendMessageW(
            state.hwnd_rows,
            LB_ADDSTRING,
            Some(WPARAM(0)),
            Some(LPARAM(wide.as_ptr() as isize)),
        );
    }
    if state
        .last_applied
        .map_or(true, |t| t.elapsed() > Duration::from_secs(2))
    {
        update_status(state);
    }
}

unsafe fn update_value(state: &mut CfgState) {
    if let Some(sel) = state.selected {
        if let Some(&idx) = state.filtered.get(sel) {
            let row = config_model::ROWS[idx];
            let v = state
                .doc
                .as_ref()
                .and_then(|d| d.staged(row.key).or(d.get(row.key)))
                .map(|v| v.to_string())
                .unwrap_or_default();
            let wide: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
            windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                state.hwnd_value,
                windows::core::PCWSTR(wide.as_ptr()),
            );
            return;
        }
    }
    windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(state.hwnd_value, w!(""));
}

unsafe fn update_status(state: &mut CfgState) {
    if let Some(t) = state.last_applied {
        if t.elapsed() < Duration::from_secs(3) {
            if let Some(k) = &state.last_applied_key {
                let txt = format!("Applied {}", k);
                let wide: Vec<u16> = txt.encode_utf16().chain(std::iter::once(0)).collect();
                windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    state.hwnd_status,
                    windows::core::PCWSTR(wide.as_ptr()),
                );
                return;
            }
        }
    }
    let txt = if state.doc.is_none() {
        "Loading...".to_string()
    } else {
        let dirty = state
            .doc
            .as_ref()
            .map(|d| d.dirty_keys().len())
            .unwrap_or(0);
        if dirty > 0 {
            format!("{} dirty - Apply/Revert", dirty)
        } else {
            "Clean".to_string()
        }
    };
    let wide: Vec<u16> = txt.encode_utf16().chain(std::iter::once(0)).collect();
    windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
        state.hwnd_status,
        windows::core::PCWSTR(wide.as_ptr()),
    );
}

unsafe extern "system" fn cfg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut CfgState;
            G_CFG = Some(ptr);
            let s = &mut *ptr;
            let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);
            s.hwnd_filter = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0),
                8,
                8,
                300,
                22,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_FILTER as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Filter:"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
                320,
                10,
                40,
                18,
                Some(hwnd),
                None,
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_tab = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("COMBOBOX"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x0003 | WS_VSCROLL.0),
                370,
                8,
                140,
                200,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_TAB as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            for t in [
                "History",
                "Popup",
                "Advanced",
                "PermanentClips",
                "Privacy",
                "Tray",
                "Startup",
                "General",
            ] {
                let wide: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
                SendMessageW(
                    s.hwnd_tab,
                    CB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(wide.as_ptr() as isize)),
                );
            }
            SendMessageW(s.hwnd_tab, CB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
            s.hwnd_rows = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | 0x0001),
                8,
                36,
                600,
                260,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_ROWS as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_value = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0),
                8,
                310,
                500,
                22,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_VALUE as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_set = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("Set"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x0000),
                520,
                310,
                60,
                22,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_SET as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_apply = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("Apply"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x0000),
                590,
                310,
                60,
                22,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_APPLY as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_revert = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("Revert"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x0000),
                660,
                310,
                60,
                22,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_REVERT as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            s.hwnd_status = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Loading..."),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
                8,
                345,
                600,
                18,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    IDC_STATUS as *mut _,
                )),
                Some(hinst),
                None,
            )
            .unwrap();
            G_CFG_EDIT_ORIG = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                s.hwnd_filter,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
            );
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                s.hwnd_filter,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
                cfg_filter_subclass as isize,
            );
            G_CFG_EDIT_ORIG2 = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                s.hwnd_value,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
            );
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                s.hwnd_value,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
                cfg_value_subclass as isize,
            );
            windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(hwnd), TIMER_CFG, 100, None);
            let _ = s.cmds.send(ipc_bridge::UiCmd::ConfigGet);
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(ptr) = G_CFG {
                let s = &mut *ptr;
                let w = (lparam.0 & 0xFFFF) as i32;
                let h = ((lparam.0 >> 16) & 0xFFFF) as i32;
                if w > 0 && h > 0 {
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_rows,
                        8,
                        36,
                        w - 16,
                        h - 90,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_value,
                        8,
                        h - 50,
                        500,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_set,
                        w - 210,
                        h - 50,
                        60,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_apply,
                        w - 140,
                        h - 50,
                        60,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_revert,
                        w - 70,
                        h - 50,
                        60,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        s.hwnd_status,
                        8,
                        h - 22,
                        w - 16,
                        18,
                        true,
                    );
                }
            }
            InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut _);
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = HBRUSH(GetStockObject(WHITE_BRUSH).0);
            FillRect(hdc, &rc, brush);
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = HBRUSH(GetStockObject(WHITE_BRUSH).0);
            FillRect(hdc, &rc, brush);
            EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_CFG {
                if let Some(ptr) = G_CFG {
                    let s = &mut *ptr;
                    for _ in 0..16 {
                        let Ok(ev) = s.events.try_recv() else {
                            break;
                        };
                        match ev {
                            ipc_bridge::UiEvent::Config(v) => {
                                let doc = config_model::ConfigDoc::from_snapshot(&v);
                                s.doc = Some(doc);
                                refresh_rows(s);
                            }
                            ipc_bridge::UiEvent::ConfigApplied(k) => {
                                let txt = format!("Applied {}", k);
                                let wide: Vec<u16> =
                                    txt.encode_utf16().chain(std::iter::once(0)).collect();
                                windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                                    s.hwnd_status,
                                    windows::core::PCWSTR(wide.as_ptr()),
                                );
                                s.last_applied = Some(Instant::now());
                                s.last_applied_key = Some(k.clone());
                            }
                            ipc_bridge::UiEvent::Error(e) => {
                                let wide: Vec<u16> =
                                    e.encode_utf16().chain(std::iter::once(0)).collect();
                                windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                                    s.hwnd_status,
                                    windows::core::PCWSTR(wide.as_ptr()),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if let Some(ptr) = G_CFG {
                let s = &mut *ptr;
                match (id, code) {
                    (IDC_FILTER, EN_CHANGE) => {
                        let mut buf = [0u16; 512];
                        let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                            s.hwnd_filter,
                            &mut buf,
                        );
                        s.filter = String::from_utf16_lossy(&buf[..len as usize]);
                        refresh_rows(s);
                    }
                    (IDC_TAB, 1) => {
                        let sel = SendMessageW(
                            s.hwnd_tab,
                            CB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        );
                        if sel.0 >= 0 {
                            let tabs = [
                                "History",
                                "Popup",
                                "Advanced",
                                "PermanentClips",
                                "Privacy",
                                "Tray",
                                "Startup",
                                "General",
                            ];
                            s.tab = tabs[sel.0 as usize].to_string();
                            refresh_rows(s);
                        }
                    }
                    (IDC_ROWS, LBN_SELCHANGE) => {
                        let sel = SendMessageW(
                            s.hwnd_rows,
                            LB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        );
                        if sel.0 >= 0 {
                            s.selected = Some(sel.0 as usize);
                            update_value(s);
                        }
                    }
                    (IDC_SET, _) => {
                        if let Some(sel) = s.selected {
                            if let Some(&idx) = s.filtered.get(sel) {
                                let row = config_model::ROWS[idx];
                                let mut buf = [0u16; 1024];
                                let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                                    s.hwnd_value,
                                    &mut buf,
                                );
                                let text = String::from_utf16_lossy(&buf[..len as usize]);
                                // Parse based on kind - for now treat as string/int via json
                                let val = match row.kind {
                                    config_model::Kind::Int => text
                                        .parse::<i64>()
                                        .map(|n| serde_json::Value::Number(n.into()))
                                        .unwrap_or(serde_json::Value::String(text)),
                                    config_model::Kind::Bool => {
                                        serde_json::Value::Bool(text == "true" || text == "1")
                                    }
                                    _ => serde_json::Value::String(text),
                                };
                                if let Some(doc) = s.doc.as_mut() {
                                    match doc.stage(row.key, val) {
                                        Ok(()) => update_status(s),
                                        Err(e) => {
                                            let wide: Vec<u16> = format!("Error: {}", e)
                                                .encode_utf16()
                                                .chain(std::iter::once(0))
                                                .collect();
                                            windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                                                s.hwnd_status,
                                                windows::core::PCWSTR(wide.as_ptr()),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    (IDC_APPLY, _) => {
                        if let Some(doc) = s.doc.as_ref() {
                            for (k, v) in doc.apply_plan() {
                                let _ = s.cmds.send(ipc_bridge::UiCmd::ConfigSet(k, v));
                            }
                            let _ = s.cmds.send(ipc_bridge::UiCmd::ConfigGet);
                        }
                    }
                    (IDC_REVERT, _) => {
                        if let Some(doc) = s.doc.as_mut() {
                            doc.revert();
                            refresh_rows(s);
                            update_status(s);
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = G_CFG {
                windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), TIMER_CFG);
                let _ = Box::from_raw(ptr);
                G_CFG = None;
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn try_connect_local(profile: &std::path::Path) -> Option<Client> {
    let socket = {
        if let Ok(text) = std::fs::read_to_string(profile.join("rendezvous.json")) {
            if let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(sock) = body.get("socket").and_then(|v| v.as_str()) {
                    let tok = std::fs::read_to_string(profile.join("token"))
                        .ok()?
                        .trim()
                        .to_string();
                    if let Ok(c) = Client::connect(&std::path::PathBuf::from(sock), &tok) {
                        return Some(c);
                    }
                }
            }
        }
        let rt = std::env::var_os("XDG_RUNTIME_DIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        framing::socket_path_for(profile, &rt)
    };
    let token = std::fs::read_to_string(profile.join("token"))
        .ok()?
        .trim()
        .to_string();
    Client::connect(&socket, &token).ok()
}

struct ClientT {
    client: Client,
}
impl ipc_bridge::Transport for ClientT {
    fn call(
        &mut self,
        m: &str,
        p: &serde_json::Value,
    ) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
        self.client.call(m, p.clone())
    }
}
struct OfflineT;
impl ipc_bridge::Transport for OfflineT {
    fn call(
        &mut self,
        _m: &str,
        _p: &serde_json::Value,
    ) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
        Err(clipmonger_ipc::proto::IpcError {
            code: "unavailable".to_string(),
            message: "daemon not running".to_string(),
            retryable: true,
            backend: None,
        })
    }
}

pub fn run_config_window(profile: PathBuf) {
    let (cmds, events) = {
        let t: Box<dyn ipc_bridge::Transport> = match try_connect_local(&profile) {
            Some(c) => Box::new(ClientT { client: c }),
            None => Box::new(OfflineT),
        };
        ipc_bridge::Bridge::spawn(t)
    };
    unsafe {
        let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);
        let cls = w!("ClipmongerConfigFallback");
        let wc = WNDCLASSW {
            hInstance: hinst,
            lpszClassName: cls,
            lpfnWndProc: Some(cfg_proc),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let state = Box::new(CfgState {
            cmds,
            events,
            doc: None,
            filter: String::new(),
            tab: "History".to_string(),
            selected: None,
            filtered: Vec::new(),
            hwnd_tab: HWND(std::ptr::null_mut()),
            hwnd_filter: HWND(std::ptr::null_mut()),
            hwnd_rows: HWND(std::ptr::null_mut()),
            hwnd_value: HWND(std::ptr::null_mut()),
            hwnd_apply: HWND(std::ptr::null_mut()),
            hwnd_revert: HWND(std::ptr::null_mut()),
            hwnd_status: HWND(std::ptr::null_mut()),
            hwnd_set: HWND(std::ptr::null_mut()),
            last_applied: None,
            last_applied_key: None,
        });
        let ptr = Box::into_raw(state);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            cls,
            w!("Clipmonger - Config (CPU) - 27 rows"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            750,
            420,
            None,
            None,
            Some(hinst),
            Some(ptr as *const _),
        )
        .unwrap();
        ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
