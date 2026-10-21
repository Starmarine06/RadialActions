//! The radial wheel window and the result toast.
//!
//! Both are created on demand and destroyed when done, so nothing is held
//! while idle. The wheel never takes focus: Explorer keeps its selection.

use crate::actions::{Kind, Node, Runner};
use crate::gfx::{self, Geom, Mask, Surface, Text};
use crate::{hook, recent, sys};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_POLL: usize = 1;
const TIMER_TOAST: usize = 2;
const POLL_MS: u32 = 16;
const TOAST_MS: u32 = 2400;

enum Act {
    Run(Runner),
    Open(usize),
    Back,
}

struct Entry {
    label: String,
    tip: String,
    act: Act,
    /// Identifies the action in the usage history; empty for Back.
    id: String,
    slow: bool,
}

struct Wheel {
    hwnd: HWND,
    surf: Surface,
    text: Text,
    geom: Geom,
    scale: f32,
    origin: POINT,
    path: PathBuf,
    /// Header text while nothing is hovered: a file name or "N files selected".
    title: String,
    /// What the usage history is filed under (a file type, or "many:pdf").
    key: String,
    root: Vec<Node>,
    trail: Vec<usize>,
    entries: Vec<Entry>,
    labels: Vec<Mask>,
    hover: Option<usize>,
    flick: bool,
}

struct Toast {
    hwnd: HWND,
    _surf: Surface,
}

thread_local! {
    static WHEEL: RefCell<Option<Wheel>> = const { RefCell::new(None) };
    static TOAST: RefCell<Option<Toast>> = const { RefCell::new(None) };
}

pub fn init(hinst: HINSTANCE) {
    unsafe {
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        for (name, proc) in [
            (w!("RadialActionsWheel"), Some(wheel_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (w!("RadialActionsToast"), Some(toast_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
        ] {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: hinst,
                lpszClassName: name,
                hCursor: arrow,
                ..Default::default()
            });
        }
    }
}

fn shift_down() -> bool {
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 }
}

fn work_area(pt: POINT) -> windows::Win32::Foundation::RECT {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        info.rcWork
    }
}

fn present(hwnd: HWND, surf: &Surface, at: POINT) {
    unsafe {
        let size = SIZE { cx: surf.w, cy: surf.h };
        let src = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&at),
            Some(&size),
            Some(surf.dc()),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

fn level<'a>(root: &'a [Node], trail: &[usize]) -> &'a [Node] {
    let mut cur = root;
    for &i in trail {
        if let Kind::Group(children) = &cur[i].kind {
            cur = children;
        }
    }
    cur
}

impl Wheel {
    fn rebuild(&mut self) {
        let nodes = level(&self.root, &self.trail);
        let mut entries: Vec<Entry> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| Entry {
                label: n.label.clone(),
                tip: n.tip.clone(),
                act: match &n.kind {
                    Kind::Run(r) => Act::Run(r.clone()),
                    Kind::Group(_) => Act::Open(i),
                },
                id: n.id.clone(),
                slow: n.slow,
            })
            .collect();
        if !self.trail.is_empty() {
            entries.push(Entry {
                label: "Back".into(),
                tip: "Back".into(),
                act: Act::Back,
                id: String::new(),
                slow: false,
            });
        }
        self.labels = entries.iter().map(|e| self.text.mask(&e.label)).collect();
        self.entries = entries;
        self.hover = None;
    }

    fn repaint(&mut self) {
        let header = match self.hover {
            Some(i) => self.entries[i].tip.clone(),
            None => self.title.clone(),
        };
        let header = self.text.mask(&header);
        let (w, h) = (self.surf.w, self.surf.h);
        gfx::draw_wheel(self.surf.pixels(), w, h, &self.geom, self.scale, self.hover, &self.labels, &header);
        present(self.hwnd, &self.surf, self.origin);
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        gfx::slice_at(&self.geom, self.entries.len(), x, y, self.flick)
    }
}

/// Opens the wheel centred on `pt` for `path`. Does nothing if already open.
pub fn show(pt: POINT, title: String, key: String, path: PathBuf, root: Vec<Node>) {
    if root.is_empty() || WHEEL.with_borrow(|w| w.is_some()) {
        return;
    }
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let (r_out, r_in, pad, head) = (120.0 * scale, 36.0 * scale, 10.0 * scale, 40.0 * scale);
    let w = (2.0 * (r_out + pad)).ceil() as i32;
    let h = (head + 2.0 * (r_out + pad)).ceil() as i32;
    let geom = Geom { cx: w as f32 / 2.0, cy: head + pad + r_out, r_in, r_out };

    let area = work_area(pt);
    let origin = POINT {
        x: (pt.x - geom.cx as i32).clamp(area.left, (area.right - w).max(area.left)),
        y: (pt.y - geom.cy as i32).clamp(area.top, (area.bottom - h).max(area.top)),
    };
    // Keep the ring centred on the cursor unless the screen edge forces a shift.
    let geom = Geom { cx: (pt.x - origin.x) as f32, cy: (pt.y - origin.y) as f32, ..geom };

    let Some(surf) = Surface::new(w, h) else { return };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("RadialActionsWheel"),
            w!(""),
            WS_POPUP,
            origin.x,
            origin.y,
            w,
            h,
            None,
            None,
            None,
            None,
        )
    };
    let Ok(hwnd) = hwnd else { return };

    let mut wheel = Wheel {
        hwnd,
        surf,
        text: Text::new((14.0 * scale) as i32, 600),
        geom,
        scale,
        origin,
        path,
        title,
        key,
        root,
        trail: Vec::new(),
        entries: Vec::new(),
        labels: Vec::new(),
        hover: None,
        flick: sys::FLICK.load(Ordering::Relaxed),
    };
    wheel.rebuild();
    wheel.repaint();
    WHEEL.with_borrow_mut(|s| *s = Some(wheel));
    hook::WHEEL_OPEN.store(true, Ordering::Release);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_POLL, POLL_MS, None);
    }
}

pub fn is_open() -> bool {
    WHEEL.with_borrow(|w| w.is_some())
}

pub fn close() {
    let Some(wheel) = WHEEL.with_borrow_mut(|s| s.take()) else { return };
    hook::WHEEL_OPEN.store(false, Ordering::Release);
    // Don't reopen on the same file while Shift is still held.
    hook::block_until_shift_up();
    unsafe {
        let _ = KillTimer(Some(wheel.hwnd), TIMER_POLL);
        let _ = DestroyWindow(wheel.hwnd);
    }
    drop(wheel);
    crate::trim_memory();
}

enum Outcome {
    Stay,
    Close,
    Pick(usize),
}

fn tick() -> Outcome {
    WHEEL.with_borrow_mut(|s| {
        let Some(w) = s.as_mut() else { return Outcome::Stay };
        if unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000 != 0 {
            return Outcome::Close;
        }
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let hover = w.hit((pt.x - w.origin.x) as f32, (pt.y - w.origin.y) as f32);
        if hover != w.hover {
            w.hover = hover;
            w.repaint();
        }
        if w.flick && !shift_down() {
            return hover.map_or(Outcome::Close, Outcome::Pick);
        }
        Outcome::Stay
    })
}

/// Runs the chosen slice: opens a sub-wheel, goes back, or executes an action.
fn pick(i: usize) {
    enum Next {
        Done,
        Run(Runner, PathBuf, String, String, bool),
    }
    let next = WHEEL.with_borrow_mut(|s| {
        let w = s.as_mut()?;
        match &w.entries.get(i)?.act {
            Act::Run(r) => Some(Next::Run(r.clone(), w.path.clone(), w.key.clone(), w.entries[i].id.clone(), w.entries[i].slow)),
            Act::Open(idx) => {
                w.trail.push(*idx);
                // A group needs a second look at the pointer, so stop waiting for Shift-up.
                w.flick = false;
                w.rebuild();
                w.repaint();
                Some(Next::Done)
            }
            Act::Back => {
                w.trail.pop();
                w.rebuild();
                w.repaint();
                Some(Next::Done)
            }
        }
    });
    if let Some(Next::Run(runner, path, key, id, slow)) = next {
        close();
        recent::record(&key, &id);
        if slow {
            toast_info("Working on it...");
        }
        std::thread::spawn(move || match runner(&path) {
            Ok(msg) => hook::post_result(true, msg),
            Err(msg) => hook::post_result(false, msg),
        });
    }
}

/// A mouse press anywhere that the wheel itself didn't claim dismisses it.
pub fn on_outside_click(pt: POINT) {
    let claimed = WHEEL.with_borrow(|s| {
        s.as_ref().is_some_and(|w| {
            let flick = false;
            gfx::slice_at(&w.geom, w.entries.len(), (pt.x - w.origin.x) as f32, (pt.y - w.origin.y) as f32, flick)
                .is_some()
        })
    });
    if !claimed {
        close();
    }
}

unsafe extern "system" fn wheel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if wp.0 == TIMER_POLL => {
            match tick() {
                Outcome::Stay => {}
                Outcome::Close => close(),
                Outcome::Pick(i) => pick(i),
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (lp.0 as i16 as f32, (lp.0 >> 16) as i16 as f32);
            if let Some(i) = WHEEL.with_borrow(|s| s.as_ref().and_then(|w| w.hit(x, y))) {
                pick(i);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub fn toast(msg: &str, ok: bool) {
    show_toast(msg, if ok { gfx::OK } else { gfx::FAIL }, TOAST_MS);
}

/// A neutral "still going" toast that stays until the result replaces it.
pub fn toast_info(msg: &str) {
    show_toast(msg, gfx::INFO, 5 * 60 * 1000);
}

fn show_toast(msg: &str, color: gfx::Rgb, ms: u32) {
    close_toast();
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let text = Text::new((14.0 * scale) as i32, 600);
    let label = text.mask(msg);
    drop(text);
    let (pw, ph) = (label.w as f32 + 36.0 * scale, label.h as f32 + 18.0 * scale);
    let (w, h) = (pw.ceil() as i32 + 4, ph.ceil() as i32 + 4);
    let Some(mut surf) = Surface::new(w, h) else { return };
    gfx::pill(surf.pixels(), w, h, (2.0, 2.0, pw, ph), color, &label);

    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let area = work_area(cursor);
    let at = POINT { x: (area.left + area.right - w) / 2, y: area.bottom - h - (60.0 * scale) as i32 };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            w!("RadialActionsToast"),
            w!(""),
            WS_POPUP,
            at.x,
            at.y,
            w,
            h,
            None,
            None,
            None,
            None,
        )
    };
    let Ok(hwnd) = hwnd else { return };
    present(hwnd, &surf, at);
    TOAST.with_borrow_mut(|t| *t = Some(Toast { hwnd, _surf: surf }));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_TOAST, ms, None);
    }
}

fn close_toast() {
    if let Some(t) = TOAST.with_borrow_mut(|t| t.take()) {
        unsafe {
            let _ = KillTimer(Some(t.hwnd), TIMER_TOAST);
            let _ = DestroyWindow(t.hwnd);
        }
        drop(t);
        crate::trim_memory();
    }
}

unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_TOAST => {
            close_toast();
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
