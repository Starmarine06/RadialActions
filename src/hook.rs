//! Shift + mouse-move trigger, tray icon and the hidden message window.
//!
//! The low-level hook only posts messages; the work happens on the message
//! loop after the cursor has rested for `DWELL_MS`, so shift-selecting in
//! Explorer isn't interrupted.

use crate::{overlay, sys};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_SHIFT_MOVE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_RESULT: u32 = WM_APP + 3;
const WM_CLICK: u32 = WM_APP + 4;
const TIMER_DWELL: usize = 1;
const TIMER_UNBLOCK: usize = 2;
const DWELL_MS: u32 = 150;

const CMD_CLICK: usize = 1;
const CMD_FLICK: usize = 2;
const CMD_AUTOSTART: usize = 3;
const CMD_QUIT: usize = 4;

static MAIN: AtomicIsize = AtomicIsize::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static ON_TRIGGER: std::sync::OnceLock<fn(POINT)> = std::sync::OnceLock::new();

/// Set while the wheel is on screen.
pub static WHEEL_OPEN: AtomicBool = AtomicBool::new(false);
/// Set after the wheel closes; cleared once Shift is let go and the mouse moves.
pub static BLOCKED: AtomicBool = AtomicBool::new(false);

fn shift_down() -> bool {
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 }
}

fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::Acquire) as _)
}

/// Suppresses new wheels until Shift has been let go once. Called when the
/// wheel closes so it can't reopen on the same file while Shift is still held.
pub fn block_until_shift_up() {
    BLOCKED.store(true, Ordering::Release);
    unsafe {
        SetTimer(Some(main_hwnd()), TIMER_UNBLOCK, 40, None);
    }
}

/// Reports an action's outcome from a worker thread to the UI thread.
pub fn post_result(ok: bool, msg: String) {
    let raw = Box::into_raw(Box::new(msg));
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
                if !shift_down() {
                    BLOCKED.store(false, Ordering::Release);
                } else if !WHEEL_OPEN.load(Ordering::Acquire)
                    && !BLOCKED.load(Ordering::Acquire)
                    && !PENDING.swap(true, Ordering::AcqRel)
                {
                    let _ = PostMessageW(Some(main_hwnd()), WM_SHIFT_MOVE, WPARAM(0), LPARAM(0));
                }
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN if WHEEL_OPEN.load(Ordering::Acquire) => {
                let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
                let _ = PostMessageW(
                    Some(main_hwnd()),
                    WM_CLICK,
                    WPARAM(info.pt.x as u32 as usize),
                    LPARAM(info.pt.y as isize),
                );
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wp, lp)
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

unsafe fn tray_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let flick = sys::FLICK.load(Ordering::Relaxed);
    let checked = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, checked(!flick), CMD_CLICK, w!("Click to pick"));
    let _ = AppendMenuW(menu, checked(flick), CMD_FLICK, w!("Flick and release Shift"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, checked(sys::autostart_enabled()), CMD_AUTOSTART, w!("Start with Windows"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu dismisses when the user clicks elsewhere.
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        CMD_CLICK | CMD_FLICK => {
            sys::FLICK.store(cmd.0 as usize == CMD_FLICK, Ordering::Relaxed);
            sys::save_config();
        }
        CMD_AUTOSTART => {
            if let Err(e) = sys::set_autostart(!sys::autostart_enabled()) {
                overlay::toast(&format!("Couldn't change startup: {e}"), false);
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
        WM_SHIFT_MOVE => {
            PENDING.store(false, Ordering::Release);
            SetTimer(Some(hwnd), TIMER_DWELL, DWELL_MS, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_UNBLOCK => {
            if !shift_down() {
                BLOCKED.store(false, Ordering::Release);
                let _ = KillTimer(Some(hwnd), TIMER_UNBLOCK);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_DWELL => {
            let _ = KillTimer(Some(hwnd), TIMER_DWELL);
            if shift_down() && !BLOCKED.load(Ordering::Acquire) && !overlay::is_open() {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    if let Some(f) = ON_TRIGGER.get() {
                        f(pt);
                    }
                }
            }
            LRESULT(0)
        }
        WM_CLICK => {
            overlay::on_outside_click(POINT { x: wp.0 as u32 as i32, y: lp.0 as i32 });
            LRESULT(0)
        }
        WM_RESULT => {
            let text = Box::from_raw(lp.0 as *mut String);
            overlay::toast(&text, wp.0 != 0);
            LRESULT(0)
        }
        WM_TRAY => {
            if lp.0 as u32 == WM_RBUTTONUP || lp.0 as u32 == WM_LBUTTONUP {
                tray_menu(hwnd);
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
pub fn run(on_trigger: fn(POINT)) -> windows::core::Result<()> {
    let _ = ON_TRIGGER.set(on_trigger);
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("RadialActionsMsg");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        });
        overlay::init(hinst.into());
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        // A hidden top-level window (not message-only) so tray callbacks arrive.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
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
        )?;
        MAIN.store(hwnd.0 as isize, Ordering::Release);
        add_tray(hwnd);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst.into()), 0)?;
        // Drop the pages touched while setting up the window and tray icon.
        crate::trim_memory();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    Ok(())
}
