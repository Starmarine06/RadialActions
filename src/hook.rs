//! Trigger key + mouse-move detection, tray icon and the hidden message window.
//!
//! The low-level mouse hook lives on its own thread and only posts messages, so
//! a slow shell query can never make the pointer stutter. Work happens on the
//! main loop after the cursor has rested, so selecting in Explorer isn't
//! interrupted.

use crate::{config, journal, overlay, sys, ui};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU8, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LBUTTON, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_SHIFT,
};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_KEY_MOVE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_RESULT: u32 = WM_APP + 3;
const WM_CLICK: u32 = WM_APP + 4;
/// Posted by the settings window after saving.
const WM_RELOAD: u32 = WM_APP + 5;
/// Posted by a second launch.
const WM_SHOW_SETTINGS: u32 = WM_APP + 6;
const TIMER_DWELL: usize = 1;
const TIMER_UNBLOCK: usize = 2;
const TIMER_TRIM: usize = 3;
/// Memory is handed back only after a quiet spell, so back-to-back uses stay snappy.
const TRIM_AFTER_MS: u32 = 20_000;

const CMD_SETTINGS: usize = 1;
const CMD_HELP: usize = 2;
const CMD_UNDO: usize = 3;
const CMD_PAUSE: usize = 4;
const CMD_AUTOSTART: usize = 5;
const CMD_QUIT: usize = 6;

const CLASS: PCWSTR = w!("RadialActionsMsg");

static MAIN: AtomicIsize = AtomicIsize::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static KEY: AtomicU8 = AtomicU8::new(0);
static DELAY: AtomicU32 = AtomicU32::new(150);
static ON_TRIGGER: std::sync::OnceLock<fn(POINT)> = std::sync::OnceLock::new();

/// Set while the wheel is on screen.
pub static WHEEL_OPEN: AtomicBool = AtomicBool::new(false);
/// Set after the wheel closes or a click; cleared once the trigger key is let go.
static BLOCKED: AtomicBool = AtomicBool::new(false);

fn down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(key.0 as i32) as u16 & 0x8000 != 0 }
}

pub fn trigger_held() -> bool {
    match KEY.load(Ordering::Relaxed) {
        1 => down(VK_CONTROL),
        2 => down(VK_MENU),
        3 => down(VK_CONTROL) && down(VK_SHIFT),
        _ => down(VK_SHIFT),
    }
}

/// Picks up the trigger settings.
fn apply_settings() {
    let s = config::load();
    let key = match s.key.as_str() {
        "ctrl" => 1,
        "alt" => 2,
        "ctrl+shift" => 3,
        _ => 0,
    };
    KEY.store(key, Ordering::Relaxed);
    DELAY.store(s.delay_ms, Ordering::Relaxed);
}

fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::Acquire) as _)
}

/// The running app's message window, seen from another process.
fn running_window() -> Option<HWND> {
    unsafe { FindWindowW(CLASS, PCWSTR::null()).ok() }
}

pub fn notify_reload() {
    if let Some(hwnd) = running_window() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_RELOAD, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn ask_running_for_settings() -> bool {
    running_window().is_some_and(|hwnd| unsafe {
        PostMessageW(Some(hwnd), WM_SHOW_SETTINGS, WPARAM(0), LPARAM(0)).is_ok()
    })
}

/// Suppresses new wheels until the trigger key has been let go once.
pub fn block_until_released() {
    BLOCKED.store(true, Ordering::Release);
    unsafe {
        SetTimer(Some(main_hwnd()), TIMER_UNBLOCK, 40, None);
    }
}

pub fn trim_later() {
    unsafe {
        SetTimer(Some(main_hwnd()), TIMER_TRIM, TRIM_AFTER_MS, None);
    }
}

struct Report {
    msg: String,
    undoable: bool,
}

/// Reports an action's outcome from a worker thread to the UI thread.
pub fn post_result(ok: bool, msg: String, undoable: bool) {
    let raw = Box::into_raw(Box::new(Report { msg, undoable }));
    unsafe {
        if PostMessageW(Some(main_hwnd()), WM_RESULT, WPARAM(ok as usize), LPARAM(raw as isize)).is_err() {
            drop(Box::from_raw(raw));
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        match wp.0 as u32 {
            WM_MOUSEMOVE => {
                if !trigger_held() {
                    BLOCKED.store(false, Ordering::Release);
                } else if !WHEEL_OPEN.load(Ordering::Acquire)
                    && !BLOCKED.load(Ordering::Acquire)
                    && !PENDING.swap(true, Ordering::AcqRel)
                {
                    let _ = PostMessageW(Some(main_hwnd()), WM_KEY_MOVE, WPARAM(0), LPARAM(0));
                }
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                if WHEEL_OPEN.load(Ordering::Acquire) {
                    let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
                    let _ = PostMessageW(
                        Some(main_hwnd()),
                        WM_CLICK,
                        WPARAM(info.pt.x as u32 as usize),
                        LPARAM(info.pt.y as isize),
                    );
                } else if trigger_held() {
                    // Range or multi selection in progress: stay out of the way.
                    BLOCKED.store(true, Ordering::Release);
                }
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn start_hook() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let hinst = GetModuleHandleW(None).ok().map(Into::into);
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hinst, 0) {
            Ok(_) => {
                let _ = tx.send(Ok(()));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
            }
        }
    });
    rx.recv().map_err(|e| e.to_string())?
}

/// Games, presentations and full-screen video shouldn't be interrupted.
fn full_screen_busy() -> bool {
    unsafe {
        SHQueryUserNotificationState()
            .is_ok_and(|s| s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
    }
}

/// Draws the tray icon once and reuses it for the life of the process.
fn tray_icon() -> HICON {
    static ICON: AtomicIsize = AtomicIsize::new(0);
    let cached = ICON.load(Ordering::Relaxed);
    if cached != 0 {
        return HICON(cached as _);
    }
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let icon = (|| unsafe {
        let mut surf = crate::gfx::Surface::new(size, size)?;
        crate::gfx::draw_icon(surf.pixels(), size);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: surf.bitmap() };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(mask.into());
        icon
    })()
    .unwrap_or_else(|| unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default());
    ICON.store(icon.0 as isize, Ordering::Relaxed);
    icon
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: tray_icon(),
        ..Default::default()
    };
    for (dst, src) in nid.szTip.iter_mut().zip("RadialActions".encode_utf16()) {
        *dst = src;
    }
    nid
}

fn add_tray(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_ui(page: &str) {
    if let Err(e) = ui::spawn(page) {
        overlay::toast(&format!("Couldn't open the window: {e}"), false, false, &config::load().theme);
    }
}

unsafe fn tray_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let checked = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, w!("Settings\u{2026}"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_HELP, w!("How it works"));
    if let Some(label) = journal::pending() {
        let text = wide(&format!("Undo: {}", label.replace('&', "&&")));
        let _ = AppendMenuW(menu, MF_STRING, CMD_UNDO, PCWSTR(text.as_ptr()));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, checked(PAUSED.load(Ordering::Relaxed)), CMD_PAUSE, w!("Pause"));
    let autostart = sys::autostart_enabled();
    let _ = AppendMenuW(menu, checked(autostart), CMD_AUTOSTART, w!("Start with Windows"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu dismisses when the user clicks elsewhere.
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        CMD_SETTINGS => open_ui("settings"),
        CMD_HELP => open_ui("welcome"),
        CMD_UNDO => overlay::undo_last(),
        CMD_PAUSE => {
            PAUSED.fetch_xor(true, Ordering::Relaxed);
        }
        CMD_AUTOSTART => {
            if let Err(e) = sys::set_autostart(!autostart) {
                overlay::toast(&format!("Couldn't change startup: {e}"), false, false, &config::load().theme);
            }
        }
        CMD_QUIT => {
            let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
            PostQuitMessage(0);
        }
        _ => {}
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
        add_tray(hwnd);
        return LRESULT(0);
    }
    match msg {
        WM_KEY_MOVE => {
            PENDING.store(false, Ordering::Release);
            // Restarted on every move, so it fires once the pointer rests.
            SetTimer(Some(hwnd), TIMER_DWELL, DELAY.load(Ordering::Relaxed), None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_UNBLOCK => {
            if !trigger_held() {
                BLOCKED.store(false, Ordering::Release);
                let _ = KillTimer(Some(hwnd), TIMER_UNBLOCK);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_DWELL => {
            let _ = KillTimer(Some(hwnd), TIMER_DWELL);
            // A held button means a drag (Shift-drag moves files).
            let dragging = down(VK_LBUTTON) || down(VK_RBUTTON) || down(VK_MBUTTON);
            if trigger_held()
                && !dragging
                && !PAUSED.load(Ordering::Relaxed)
                && !BLOCKED.load(Ordering::Acquire)
                && !overlay::is_open()
                && !(config::load().pause_fullscreen && full_screen_busy())
            {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    if let Some(f) = ON_TRIGGER.get() {
                        f(pt);
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TRIM => {
            let _ = KillTimer(Some(hwnd), TIMER_TRIM);
            if !overlay::is_open() {
                crate::trim_memory();
            }
            LRESULT(0)
        }
        WM_CLICK => {
            overlay::on_outside_click(POINT { x: wp.0 as u32 as i32, y: lp.0 as i32 });
            LRESULT(0)
        }
        WM_RESULT => {
            let report = Box::from_raw(lp.0 as *mut Report);
            overlay::toast(&report.msg, wp.0 != 0, report.undoable, &config::load().theme);
            trim_later();
            LRESULT(0)
        }
        WM_RELOAD => {
            apply_settings();
            LRESULT(0)
        }
        WM_SHOW_SETTINGS => {
            open_ui("settings");
            LRESULT(0)
        }
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP => open_ui("settings"),
                WM_RBUTTONUP => tray_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Installs the hook and runs the message loop until the user quits.
pub fn run(on_trigger: fn(POINT)) -> Result<(), String> {
    let _ = ON_TRIGGER.set(on_trigger);
    apply_settings();
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            ..Default::default()
        });
        overlay::init(hinst.into());
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        // A hidden top-level window (not message-only) so tray callbacks arrive.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS,
            w!("RadialActions"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        MAIN.store(hwnd.0 as isize, Ordering::Release);
        add_tray(hwnd);
        start_hook()?;
        trim_later();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
