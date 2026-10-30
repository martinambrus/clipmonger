#![allow(unsafe_code)]
use crate::{ipc_bridge, popup, strings};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM, WPARAM as WParamInner};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetStockObject, InvalidateRect, PAINTSTRUCT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    NOTIFYICONDATAW_0,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, GetCursorPos, SetForegroundWindow as SetFgWin, TrackPopupMenu,
    MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, KillTimer,
    RegisterClassW, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    EN_CHANGE, ES_AUTOHSCROLL, HMENU, LBN_DBLCLK, LBN_SELCHANGE, MSG, SW_HIDE, SW_SHOW,
    WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONUP,
    WM_PAINT, WM_RBUTTONUP, WM_SETFOCUS, WM_SIZE, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE, WS_VSCROLL,
};
const POPUP_SIZE: (u32, u32) = (480, 360);
const IDC_SEARCH: i32 = 101;
const IDC_LIST: i32 = 102;
const IDC_STATUS: i32 = 103;
const IDC_CONFIG: i32 = 104;
const TIMER_IPC: usize = 1;
const TIMER_QUIT: usize = 2;
const WM_TRAY: u32 = WM_APP + 1;
const ID_TRAY_SHOW: usize = 1001;
const ID_TRAY_SETTINGS: usize = 1002;
const ID_TRAY_EXIT: usize = 1003;
struct PopupState {
    vm: popup::PopupVm,
    cmds: mpsc::Sender<ipc_bridge::UiCmd>,
    events: mpsc::Receiver<ipc_bridge::UiEvent>,
    config: Arc<Mutex<crate::ConfigWindowState>>,
    hwnd_main: HWND,
    hwnd_search: HWND,
    hwnd_list: HWND,
    hwnd_status: HWND,
    hwnd_config: HWND,
    last_poll: Instant,
    should_exit: bool,
}
static mut G_STATE: Option<*mut PopupState> = None;
static mut G_ORIG_EDIT: isize = 0;
static mut G_ORIG_LIST: isize = 0;
static mut G_TARGET_HWND: Option<windows::Win32::Foundation::HWND> = None;
static mut G_HOTKEY_FALLBACK: bool = false;
static mut G_HOTKEY_HOOK: Option<windows::Win32::UI::WindowsAndMessaging::HHOOK> = None;
static mut G_HOTKEY_HWND: Option<HWND> = None;
static mut G_TRAY_HWND: Option<HWND> = None;
static mut G_HOTKEY_PREV_DOWN: bool = false;
static mut G_HOTKEY_VK: u32 = 0x48;
static mut G_HOTKEY_CTRL: bool = true;
static mut G_HOTKEY_ALT: bool = true;
static mut G_HOTKEY_SHIFT: bool = false;
fn parse_hotkey(hotkey: &str) -> (u32, bool, bool, bool) {
    let lower = hotkey.to_ascii_lowercase();
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut vk: u32 = 0x48;
    for part in lower.split('+') {
        let p = part.trim();
        match p {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "win" | "super" | "meta" => {}
            key if key.len() == 1 => {
                if let Some(c) = key.chars().next() {
                    vk = (c.to_ascii_uppercase() as u32) & 0xFF;
                }
            }
            _ => {}
        }
    }
    (vk, ctrl, alt, shift)
}
unsafe extern "system" fn edit_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN || msg == 0x0102 {
        let vk = wparam.0 as i32;
        if vk == 0x41 || vk == 0x61 || wparam.0 == 1 {
            let ctrl = (windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32,
            ) as u16
                & 0x8000)
                != 0
                || (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                    windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32,
                ) as u16
                    & 0x8000)
                    != 0;
            if ctrl || wparam.0 == 1 {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    windows::Win32::UI::Controls::EM_SETSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(-1)),
                );
                return LRESULT(0);
            }
        }
    }
    if msg == WM_KEYDOWN {
        let vk = wparam.0 as i32;
        if matches!(vk, 0x26 | 0x28 | 0x1B | 0x0D | 0x09) {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                match vk {
                    0x1B => {
                        let par = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd)
                            .unwrap_or(HWND(std::ptr::null_mut()));
                        windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                            par,
                            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                        );
                        return LRESULT(0);
                    }
                    0x26 => {
                        if state.vm.selected > 0 {
                            state.vm.selected -= 1;
                            sync_selection(state);
                        }
                        return LRESULT(0);
                    }
                    0x28 => {
                        let len = state.vm.visible().len();
                        if state.vm.selected + 1 < len {
                            state.vm.selected += 1;
                            sync_selection(state);
                        }
                        return LRESULT(0);
                    }
                    0x0D => {
                        // Sync from listbox sel before paste (in case vm.selected stale after blur)
                        let list_sel = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                            state.hwnd_list,
                            windows::Win32::UI::WindowsAndMessaging::LB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        );
                        if list_sel.0 >= 0 {
                            let vis_len = state.vm.visible().len();
                            if (list_sel.0 as usize) < vis_len {
                                state.vm.selected = list_sel.0 as usize;
                            }
                        }
                        do_paste(state);
                        return LRESULT(0);
                    }
                    0x09 => {
                        let focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                        if focus == state.hwnd_search {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_list,
                            ));
                        } else if focus == state.hwnd_list {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_config,
                            ));
                        } else {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_search,
                            ));
                        }
                        return LRESULT(0);
                    }
                    _ => {}
                }
            }
        }
    }
    windows::Win32::UI::WindowsAndMessaging::CallWindowProcW(
        std::mem::transmute(G_ORIG_EDIT),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}
unsafe extern "system" fn list_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let vk = wparam.0 as i32;
        if vk == 0x1B {
            let par = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or(hwnd);
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                par,
                windows::Win32::UI::WindowsAndMessaging::SW_MINIMIZE,
            );
            return LRESULT(0);
        }
        if vk == 0x09 {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                let focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                if focus == hwnd {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                        state.hwnd_config,
                    ));
                } else if focus == state.hwnd_config {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                        state.hwnd_search,
                    ));
                } else {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
                }
                return LRESULT(0);
            }
        }
        if vk == 0x0D {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                do_paste(state);
                return LRESULT(0);
            }
        }
        if vk == 0x2E {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                if let Some(c) = state.vm.clip_at(state.vm.selected) {
                    let id = c.id;
                    let _ = state.cmds.send(crate::ipc_bridge::UiCmd::DeleteClip(id));
                }
                return LRESULT(0);
            }
        }
    }
    windows::Win32::UI::WindowsAndMessaging::CallWindowProcW(
        std::mem::transmute(G_ORIG_LIST),
        hwnd,
        msg,
        wparam,
        lparam,
    )
}
unsafe extern "system" fn ll_hook_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0
        && (w_param.0 as u32 == windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN
            || w_param.0 as u32 == windows::Win32::UI::WindowsAndMessaging::WM_SYSKEYDOWN)
    {
        let kb = &*(l_param.0 as *const windows::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT);
        if kb.vkCode == unsafe { G_HOTKEY_VK } {
            let ctrl = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32,
            ) as u16
                & 0x8000)
                != 0;
            let alt = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU.0 as i32,
            ) as u16
                & 0x8000)
                != 0;
            let shift = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT.0 as i32,
            ) as u16
                & 0x8000)
                != 0;
            let want_ctrl = unsafe { G_HOTKEY_CTRL };
            let want_alt = unsafe { G_HOTKEY_ALT };
            let want_shift = unsafe { G_HOTKEY_SHIFT };
            if ctrl == want_ctrl && alt == want_alt && shift == want_shift {
                if let Some(hwnd) = unsafe { G_HOTKEY_HWND } {
                    windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        Some(hwnd),
                        windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY,
                        WPARAM(1),
                        LPARAM(0),
                    );
                    return LRESULT(1);
                }
            }
        }
    }
    windows::Win32::UI::WindowsAndMessaging::CallNextHookEx(None, n_code, w_param, l_param)
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut PopupState;
            G_STATE = Some(state_ptr);
            let state = &mut *state_ptr;
            let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);
            state.hwnd_search = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32),
                8,
                8,
                464,
                22,
                Some(hwnd),
                Some(HMENU(IDC_SEARCH as *mut _)),
                Some(hinst),
                None,
            )
            .unwrap();
            state.hwnd_list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                w!(""),
                WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | 0x0001, /*LBS_NOTIFY*/
                ),
                8,
                36,
                464,
                280,
                Some(hwnd),
                Some(HMENU(IDC_LIST as *mut _)),
                Some(hinst),
                None,
            )
            .unwrap();
            state.hwnd_status = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Clipmonger - CPU fallback (no OpenGL needed)"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
                8,
                322,
                464,
                16,
                Some(hwnd),
                Some(HMENU(IDC_STATUS as *mut _)),
                Some(hinst),
                None,
            )
            .unwrap();
            state.hwnd_config = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("Config"),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x00010000 | 0x00000000),
                392,
                320,
                80,
                22,
                Some(hwnd),
                Some(HMENU(IDC_CONFIG as *mut _)),
                Some(hinst),
                None,
            )
            .unwrap();
            eprintln!("Config button created hwnd={:?}", state.hwnd_config);
            SetTimer(Some(hwnd), TIMER_IPC, 100, None);
            G_ORIG_EDIT = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                state.hwnd_search,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
            );
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                state.hwnd_search,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
                edit_subclass as isize,
            );
            G_ORIG_LIST = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                state.hwnd_list,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
            );
            windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                state.hwnd_list,
                windows::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
                list_subclass as isize,
            );
            refresh_list(state);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_SHOWWINDOW => {
            if wparam.0 != 0 {
                if let Some(ptr) = G_STATE {
                    let s = &mut *ptr;
                    s.vm.query.clear();
                    s.vm.set_target_valid(true);
                    let _ = s.cmds.send(crate::ipc_bridge::UiCmd::ListHistory);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SIZE => {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                let w = (lparam.0 & 0xFFFF) as i32;
                let h = ((lparam.0 >> 16) & 0xFFFF) as i32;
                if w > 0 && h > 0 {
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        state.hwnd_search,
                        8,
                        8,
                        w - 16,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        state.hwnd_list,
                        8,
                        36,
                        w - 16,
                        h - 70,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        state.hwnd_config,
                        w - 88,
                        h - 30,
                        80,
                        22,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        state.hwnd_status,
                        8,
                        h - 18,
                        w - 100,
                        16,
                        true,
                    );
                }
            }
            // Force full repaint on resize to avoid grey stripes
            windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = windows::Win32::Graphics::Gdi::HBRUSH(GetStockObject(WHITE_BRUSH).0);
            FillRect(hdc, &rc, brush);
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = windows::Win32::Graphics::Gdi::HBRUSH(GetStockObject(WHITE_BRUSH).0);
            FillRect(hdc, &rc, brush);
            EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            if id as usize == ID_TRAY_SHOW {
                // Opened from the tray: the foreground is the taskbar, not a
                // paste target; paste goes to whatever Windows activates.
                unsafe {
                    G_TARGET_HWND = None;
                }
                unsafe {
                    ShowWindow(hwnd, SW_SHOW);
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                    );
                    windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                }
                return LRESULT(0);
            }
            if id as usize == ID_TRAY_SETTINGS {
                if let Some(ptr) = G_STATE {
                    let s = &mut *ptr;
                    let profile = s.config.lock().unwrap().profile.clone();
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                    );
                    std::thread::spawn(move || crate::fallback_config::run_config_window(profile));
                }
                return LRESULT(0);
            }
            if id as usize == ID_TRAY_EXIT {
                unsafe {
                    if let Some(th) = G_TRAY_HWND {
                        let mut nid = NOTIFYICONDATAW::default();
                        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                        nid.hWnd = th;
                        nid.uID = 1;
                        let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
                        G_TRAY_HWND = None;
                    }
                }
                // Exit stops the daemon too (like the main shell's tray Quit),
                // which also ends an elevated helper session. Quit on the
                // Stopped reply; the timer only covers a daemon that never
                // answers (it outlasts the longest in-flight paste call).
                if let Some(ptr) = G_STATE {
                    let s = &mut *ptr;
                    let _ = s.cmds.send(crate::ipc_bridge::UiCmd::Stop);
                }
                SetTimer(Some(hwnd), TIMER_QUIT, 6000, None);
                return LRESULT(0);
            }
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                match (id, code) {
                    (IDC_SEARCH, EN_CHANGE) => {
                        let mut buf = [0u16; 512];
                        let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                            state.hwnd_search,
                            &mut buf,
                        );
                        let q = String::from_utf16_lossy(&buf[..len as usize]);
                        state.vm.query = q;
                        state.vm.selected = 0;
                        refresh_list(state);
                    }
                    (IDC_LIST, LBN_SELCHANGE) => {
                        let sel = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                            state.hwnd_list,
                            windows::Win32::UI::WindowsAndMessaging::LB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        );
                        eprintln!(
                            "LBN_SELCHANGE sel={} vm.sel before={}",
                            sel.0, state.vm.selected
                        );
                        if sel.0 >= 0 {
                            let vis = state.vm.visible();
                            if (sel.0 as usize) < vis.len() {
                                state.vm.selected = sel.0 as usize;
                                eprintln!("  vm.sel after={}", state.vm.selected);
                            }
                        }
                    }
                    (IDC_LIST, LBN_DBLCLK) => do_paste(state),
                    (IDC_CONFIG, _) => {
                        let profile = state.config.lock().unwrap().profile.clone();
                        windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                            hwnd,
                            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                        );
                        std::thread::spawn(move || {
                            crate::fallback_config::run_config_window(profile)
                        });
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(ptr) = G_STATE {
                let state = &mut *ptr;
                match wparam.0 as i32 {
                    0x1B => {
                        // Minimize to taskbar so user can restore without hotkey (degraded mode)
                        windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                            hwnd,
                            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                        );
                    }
                    0x0D => {
                        let list_sel = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                            state.hwnd_list,
                            windows::Win32::UI::WindowsAndMessaging::LB_GETCURSEL,
                            Some(WPARAM(0)),
                            Some(LPARAM(0)),
                        );
                        if list_sel.0 >= 0 {
                            let vis_len = state.vm.visible().len();
                            if (list_sel.0 as usize) < vis_len {
                                state.vm.selected = list_sel.0 as usize;
                            }
                        }
                        do_paste(state);
                    }
                    0x26 => {
                        if state.vm.selected > 0 {
                            state.vm.selected -= 1;
                            sync_selection(state);
                        }
                    }
                    0x28 => {
                        let len = state.vm.visible().len();
                        if state.vm.selected + 1 < len {
                            state.vm.selected += 1;
                            sync_selection(state);
                        }
                    }
                    0x2E => {
                        if let Some(c) = state.vm.clip_at(state.vm.selected) {
                            let id = c.id;
                            let _ = state.cmds.send(crate::ipc_bridge::UiCmd::DeleteClip(id));
                        }
                    }
                    0x09 => {
                        let focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                        if focus == state.hwnd_search {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_list,
                            ));
                        } else if focus == state.hwnd_list {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_config,
                            ));
                        } else {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                state.hwnd_search,
                            ));
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_QUIT {
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            }
            if wparam.0 == TIMER_IPC {
                if let Some(ptr) = G_STATE {
                    let state = &mut *ptr;
                    drain_events(state);
                    if state.should_exit {
                        windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                    }
                }
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            static mut LAST_TOGGLE: Option<std::time::Instant> = None;
            let now = std::time::Instant::now();
            if let Some(last) = unsafe { LAST_TOGGLE } {
                if now.duration_since(last) < std::time::Duration::from_millis(300) {
                    return LRESULT(0);
                }
            }
            unsafe {
                LAST_TOGGLE = Some(now);
            }
            // ArsClip-style foreground steal: AttachThreadInput to foreground thread
            let visible = windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool();
            let minimized = windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool();
            if visible && !minimized {
                windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                );
            } else {
                // The window the user pressed the hotkey in is the paste
                // target (also for elevated windows: reading the handle is
                // not subject to UIPI).
                let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
                G_TARGET_HWND = if fg.0.is_null() || fg == hwnd {
                    None
                } else {
                    Some(fg)
                };
                if let Some(ptr) = G_STATE {
                    let s = &mut *ptr;
                    s.vm.query.clear();
                    windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                        s.hwnd_search,
                        windows::core::w!(""),
                    )
                    .ok();
                    s.vm.set_target_valid(true);
                    let _ = s.cmds.send(crate::ipc_bridge::UiCmd::ListHistory);
                }
                windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
                );
                windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                );
                unsafe {
                    let fg_thread =
                        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None);
                    let cur_thread = windows::Win32::System::Threading::GetCurrentThreadId();
                    if fg_thread != cur_thread {
                        windows::Win32::System::Threading::AttachThreadInput(
                            fg_thread, cur_thread, true,
                        );
                        windows::Win32::UI::WindowsAndMessaging::BringWindowToTop(hwnd);
                        windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                        windows::Win32::System::Threading::AttachThreadInput(
                            fg_thread, cur_thread, false,
                        );
                    } else {
                        windows::Win32::UI::WindowsAndMessaging::BringWindowToTop(hwnd);
                        windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                    }
                }
                if let Some(ptr) = G_STATE {
                    let s = &mut *ptr;
                    let _ =
                        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(s.hwnd_search));
                }
            }
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lparam.0 as u32) & 0xFFFF;
            if event == WM_LBUTTONUP {
                // Toggle popup like hotkey
                let visible =
                    windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool();
                let minimized = windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool();
                if visible && !minimized {
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                    );
                } else {
                    // Opened from the tray: no paste target (see ID_TRAY_SHOW).
                    G_TARGET_HWND = None;
                    if let Some(ptr) = G_STATE {
                        let s = &mut *ptr;
                        s.vm.set_target_valid(true);
                    }
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
                    );
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                    );
                    windows::Win32::UI::WindowsAndMessaging::BringWindowToTop(hwnd);
                    windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                    if let Some(ptr) = G_STATE {
                        let s = &mut *ptr;
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                            s.hwnd_search,
                        ));
                    }
                }
            } else if event == WM_RBUTTONUP {
                unsafe {
                    let hmenu = CreatePopupMenu().unwrap();
                    let _ = AppendMenuW(hmenu, MF_STRING, ID_TRAY_SHOW as usize, w!("Show Popup"));
                    let _ = AppendMenuW(hmenu, MF_STRING, ID_TRAY_SETTINGS, w!("Settings"));
                    let _ = AppendMenuW(hmenu, MF_STRING, ID_TRAY_EXIT, w!("Exit"));
                    let mut pt = windows::Win32::Foundation::POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = SetFgWin(hwnd);
                    let _ = TrackPopupMenu(
                        hmenu,
                        TPM_BOTTOMALIGN | TPM_LEFTALIGN,
                        pt.x,
                        pt.y,
                        Some(0),
                        hwnd,
                        None,
                    );
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let active = (wparam.0 & 0xFFFF) as u32;
            if active == 0 {
                if windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool() {
                    windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                    );
                }
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        WM_ACTIVATEAPP => {
            if wparam.0 == 0
                && windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool()
            {
                windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                );
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        WM_CLOSE => {
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
            );
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                if let Some(th) = G_TRAY_HWND {
                    let mut nid = NOTIFYICONDATAW::default();
                    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                    nid.hWnd = th;
                    nid.uID = 1;
                    let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
                    G_TRAY_HWND = None;
                }
                if let Some(hook) = G_HOTKEY_HOOK {
                    windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(hook);
                    G_HOTKEY_HOOK = None;
                }
            }
            if let Some(ptr) = G_STATE {
                KillTimer(Some(hwnd), TIMER_IPC);
                let _ = Box::from_raw(ptr);
                G_STATE = None;
            }
            windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
fn drain_events(state: &mut PopupState) {
    unsafe {
        static mut LH: Option<Instant> = None;
        let now = Instant::now();
        let visible =
            windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(state.hwnd_main).as_bool();
        if visible && LH.map_or(true, |l| now.duration_since(l) >= Duration::from_secs(1)) {
            LH = Some(now);
            let _ = state.cmds.send(crate::ipc_bridge::UiCmd::ListHistory);
        } else if !visible && LH.map_or(true, |l| now.duration_since(l) >= Duration::from_secs(3)) {
            LH = Some(now);
            let _ = state.cmds.send(crate::ipc_bridge::UiCmd::ListHistory);
        }
    }
    for _ in 0..32 {
        let Ok(ev) = state.events.try_recv() else {
            break;
        };
        match ev {
            ipc_bridge::UiEvent::Status(b) => {
                state.vm.set_paused(
                    b.get("monitoring_paused")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                );
                state.vm.set_offline(false);
            }
            ipc_bridge::UiEvent::History(b) => {
                let prev_id = state.vm.clip_at(state.vm.selected).map(|c| c.id);
                if state.vm.apply_history(&b) {
                    let c = state.vm.clips().first().cloned();
                    state.vm.set_current(c);
                    if let Some(pid) = prev_id {
                        if let Some(pos) =
                            state.vm.visible().iter().position(|i| {
                                state.vm.clips().get(*i).map_or(false, |cl| cl.id == pid)
                            })
                        {
                            state.vm.selected = pos;
                        }
                    }
                }
                refresh_list(state);
            }
            ipc_bridge::UiEvent::Deleted(id) => {
                state.vm.remove_clip(id);
                refresh_list(state);
            }
            ipc_bridge::UiEvent::Pinned(id, p) => {
                state.vm.apply_pin(id, p);
                refresh_list(state);
            }
            ipc_bridge::UiEvent::Stopped => state.should_exit = true,
            ipc_bridge::UiEvent::PasteFailed(message) => {
                // Own thread: a modal box here would re-enter this timer handler.
                std::thread::spawn(move || unsafe {
                    let text: Vec<u16> = format!("Paste failed.\n\n{message}")
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        None,
                        windows::core::PCWSTR(text.as_ptr()),
                        w!("Clipmonger"),
                        windows::Win32::UI::WindowsAndMessaging::MB_OK
                            | windows::Win32::UI::WindowsAndMessaging::MB_ICONWARNING
                            | windows::Win32::UI::WindowsAndMessaging::MB_TOPMOST
                            | windows::Win32::UI::WindowsAndMessaging::MB_SETFOREGROUND,
                    );
                });
            }
            ipc_bridge::UiEvent::PasteUnavailable => unsafe {
                windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    Some(state.hwnd_list),
                    w!("Paste target changed - stale-target blocked"),
                    w!("Clipmonger"),
                    windows::Win32::UI::WindowsAndMessaging::MB_OK,
                );
            },
            _ => {}
        }
    }
    if state.last_poll.elapsed() >= Duration::from_secs(30) {
        state.last_poll = Instant::now();
        let _ = state.cmds.send(ipc_bridge::UiCmd::Status);
    }
    update_status(state);
}
fn refresh_list(state: &mut PopupState) {
    unsafe {
        let top = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            state.hwnd_list,
            windows::Win32::UI::WindowsAndMessaging::LB_GETTOPINDEX,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0;
        let had_focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == state.hwnd_list;
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            state.hwnd_list,
            windows::Win32::UI::WindowsAndMessaging::WM_SETREDRAW,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            state.hwnd_list,
            windows::Win32::UI::WindowsAndMessaging::LB_RESETCONTENT,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        let vis = state.vm.visible();
        for idx in vis.iter() {
            if let Some(clip) = state.vm.clips().get(*idx) {
                let mut label = clip.preview.chars().take(90).collect::<String>();
                if label.is_empty() {
                    label = format!("[{}] {}", clip.formats.join(","), clip.id);
                }
                if clip.pinned {
                    label = format!("📌 {}", label);
                }
                let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                    state.hwnd_list,
                    windows::Win32::UI::WindowsAndMessaging::LB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(wide.as_ptr() as isize)),
                );
            }
        }
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            state.hwnd_list,
            windows::Win32::UI::WindowsAndMessaging::WM_SETREDRAW,
            Some(WPARAM(1)),
            Some(LPARAM(0)),
        );
        if top >= 0 {
            let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                state.hwnd_list,
                windows::Win32::UI::WindowsAndMessaging::LB_SETTOPINDEX,
                Some(WPARAM(top as usize)),
                Some(LPARAM(0)),
            );
        }
        sync_selection(state);
        if had_focus {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(state.hwnd_list));
        }
        windows::Win32::Graphics::Gdi::InvalidateRect(Some(state.hwnd_list), None, true);
    }
    update_status(state);
}
fn sync_selection(state: &mut PopupState) {
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            state.hwnd_list,
            windows::Win32::UI::WindowsAndMessaging::LB_SETCURSEL,
            Some(WPARAM(state.vm.selected)),
            Some(LPARAM(0)),
        );
    }
}
fn update_status(state: &mut PopupState) {
    let banner = state.vm.banner();
    let status = if !banner.is_empty() {
        banner.to_string()
    } else {
        state.vm.status_line()
    };
    let wide: Vec<u16> = status.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(state.hwnd_status, w!(""));
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
            state.hwnd_status,
            windows::core::PCWSTR(wide.as_ptr()),
        );
    }
}
fn do_paste(state: &mut PopupState) {
    eprintln!(
        "PASTE sel={} vis={:?}",
        state.vm.selected,
        state.vm.visible()
    );
    let Some(clip) = state.vm.clip_at(state.vm.selected) else {
        eprintln!("  clip_at None");
        return;
    };
    let id = clip.id;
    eprintln!("  paste id={} preview={}", id, clip.preview);
    let hwnd_main = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetParent(state.hwnd_search)
            .unwrap_or(HWND(std::ptr::null_mut()))
    };
    // Target: the window the hotkey was pressed in. Sent even if it has
    // closed meanwhile, so the daemon refuses visibly instead of pasting
    // into whatever window is in front.
    let target = unsafe { G_TARGET_HWND }.filter(|t| *t != hwnd_main);
    let picked_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    unsafe {
        // Still the foreground window here: let the daemon (or the elevated
        // helper) hand focus back to the target if Windows activates some
        // other window when the popup hides.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    // Hide popup first so paste goes to previous window, not filter input (SW_HIDE for frameless POPUP, not MINIMIZE)
    if !hwnd_main.0.is_null() {
        let ok = unsafe {
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                hwnd_main,
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
            )
        };
        eprintln!("  hide ok={}", ok.as_bool());
    }
    // Picking a clip is the user gesture an admin-window paste requires.
    let _ = state.cmds.send(ipc_bridge::UiCmd::HotkeyFired);
    if let Some(target) = target {
        // The daemon waits for (or restores) focus on exactly this window,
        // so no fixed delay is needed and a paste never lands elsewhere.
        let _ = state.cmds.send(ipc_bridge::UiCmd::PasteClipTo(
            id,
            Some(target.0 as usize as u64),
            picked_ms,
        ));
        eprintln!("  paste sent to target {target:?}");
    } else {
        // Tray-opened popup: let Windows activate the previous window first.
        let cmds = state.cmds.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = cmds.send(ipc_bridge::UiCmd::PasteClipTo(id, None, picked_ms));
            eprintln!("  paste sent after hide");
        });
    }
}
pub fn run_fallback(
    cmds: mpsc::Sender<ipc_bridge::UiCmd>,
    events: mpsc::Receiver<ipc_bridge::UiEvent>,
    config: Arc<Mutex<crate::ConfigWindowState>>,
) -> std::process::ExitCode {
    let mut vm = popup::PopupVm::new();
    vm.set_offline(false);
    let state = Box::new(PopupState {
        vm,
        cmds,
        events,
        config,
        hwnd_main: HWND(std::ptr::null_mut()),
        hwnd_search: HWND::default(),
        hwnd_list: HWND::default(),
        hwnd_status: HWND::default(),
        hwnd_config: HWND::default(),
        last_poll: Instant::now(),
        should_exit: false,
    });
    let state_ptr = Box::into_raw(state);
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class_name = w!("ClipmongerFallback");
        let wc = WNDCLASSW {
            hInstance: hinstance,
            lpszClassName: class_name,
            lpfnWndProc: Some(wndproc),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // T-905: frameless on-top popup, centered via placement logic
        let (px, py) = {
            let monitors = {
                let w = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
                );
                let h = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
                );
                let rect = crate::placement::Rect {
                    x: 0,
                    y: 0,
                    w: w.max(800),
                    h: h.max(600),
                };
                vec![rect]
            };
            let (x, y) = crate::placement::place(
                crate::placement::Placement::Center,
                (0, 0),
                &monitors,
                (POPUP_SIZE.0 as i32, POPUP_SIZE.1 as i32),
            );
            (x, y)
        };
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST
                | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW,
            class_name,
            w!("Clipmonger - Popup (CPU, no OpenGL) - RustDesk OK"),
            windows::Win32::UI::WindowsAndMessaging::WS_POPUP
                | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE
                | windows::Win32::UI::WindowsAndMessaging::WS_THICKFRAME
                | windows::Win32::UI::WindowsAndMessaging::WS_SYSMENU,
            px,
            py,
            POPUP_SIZE.0 as i32,
            POPUP_SIZE.1 as i32,
            None,
            None,
            Some(hinstance),
            Some(state_ptr as *const std::ffi::c_void),
        )
        .unwrap();
        let hotkey_str = {
            let cfg = std::fs::read_to_string(
                (*(*state_ptr).config.lock().unwrap())
                    .profile
                    .join("config.toml"),
            )
            .unwrap_or_default();
            let hotkey = cfg
                .lines()
                .find(|l| l.trim_start().starts_with("hotkey"))
                .and_then(|l| l.split('=').nth(1))
                .map(|v| v.trim().trim_matches('"').to_string())
                .unwrap_or_else(|| "ctrl+alt+h".to_string());
            hotkey
        };
        let (vk, want_ctrl, want_alt, want_shift) = parse_hotkey(&hotkey_str);
        unsafe {
            G_HOTKEY_VK = vk;
            G_HOTKEY_CTRL = want_ctrl;
            G_HOTKEY_ALT = want_alt;
            G_HOTKEY_SHIFT = want_shift;
        }
        let mut mods = windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(0);
        if want_ctrl {
            mods.0 |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL.0;
        }
        if want_alt {
            mods.0 |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_ALT.0;
        }
        if want_shift {
            mods.0 |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT.0;
        }
        unsafe {
            G_TRAY_HWND = Some(hwnd);
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = windows::Win32::UI::WindowsAndMessaging::LoadIconW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION,
            )
            .unwrap_or_default();
            let tip: Vec<u16> = format!("Clipmonger - {}", hotkey_str)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            for (i, c) in tip.iter().enumerate() {
                if i < 128 {
                    nid.szTip[i] = *c;
                }
            }
            let _ = Shell_NotifyIconW(NIM_ADD, &mut nid);
        }
        let hk_ok =
            windows::Win32::UI::Input::KeyboardAndMouse::RegisterHotKey(Some(hwnd), 1, mods, vk);
        if hk_ok.is_ok() {
            eprintln!(
                "hotkey {} registered - window hidden until hotkey",
                hotkey_str
            );
            unsafe {
                G_HOTKEY_FALLBACK = false;
            }
        } else {
            let err = windows::Win32::Foundation::GetLastError();
            eprintln!(
                "hotkey {} register failed err={:?} (degraded, LL hook, start hidden)",
                hotkey_str, err
            );
            unsafe {
                G_HOTKEY_FALLBACK = true;
                G_HOTKEY_HWND = Some(hwnd);
                match windows::Win32::UI::WindowsAndMessaging::SetWindowsHookExW(
                    windows::Win32::UI::WindowsAndMessaging::WH_KEYBOARD_LL,
                    Some(ll_hook_proc),
                    None,
                    0,
                ) {
                    Ok(hook) => {
                        if hook.is_invalid() {
                            eprintln!("LL hook invalid");
                        } else {
                            eprintln!("LL hook installed for {}", hotkey_str);
                            G_HOTKEY_HOOK = Some(hook);
                        }
                    }
                    Err(e) => {
                        eprintln!("LL hook install failed {:?}", e);
                    }
                }
            }
        }
        ShowWindow(hwnd, SW_HIDE);
        if let Some(ptr) = G_STATE {
            let s = &mut *ptr;
            let _ = s.cmds.send(ipc_bridge::UiCmd::ListHistory);
            let _ = s.cmds.send(ipc_bridge::UiCmd::Status);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    std::process::ExitCode::SUCCESS
}
