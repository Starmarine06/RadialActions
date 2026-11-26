//! The radial wheel window and the result toast.
//!
//! Both are created on demand and destroyed when done, so nothing is held
//! while idle. The wheel never takes focus: Explorer keeps its selection.

use crate::actions::{self, Kind, Node, Runner};
use crate::config::Settings;
use crate::gfx::{self, Geom, Mask, Surface, Text};
use crate::theme::{self, Palette};
use crate::{hook, journal, recent};
use std::cell::RefCell;
use std::path::PathBuf;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_POLL: usize = 1;
const TIMER_TOAST: usize = 2;
const POLL_MS: u32 = 15;
const TOAST_MS: u32 = 2600;
const UNDO_MS: u32 = 7000;
/// Steps of the opening animation, one per timer tick.
const FRAMES: u32 = 8;

enum Act {
    Run(Runner),
    Open(usize),
}

struct Entry {
    label: String,
    tip: String,
    act: Act,
    /// Identifies the action in the usage history.
    id: String,
    slow: bool,
    app: bool,
}

struct Wheel {
    hwnd: HWND,
    surf: Surface,
    view: View,
    origin: POINT,
    path: PathBuf,
    /// What the usage history is filed under (a file type, or "many:pdf").
    key: String,
    root: Vec<Node>,
    trail: Vec<usize>,
    entries: Vec<Entry>,
    flick: bool,
    frame: u32,
    /// The wheel opens under the pointer, so the centre lights up only after
    /// the pointer has left it once.
    left_center: bool,
}

/// Everything needed to paint, with no window attached.
struct View {
    text: Text,
    icons: Text,
    palette: Palette,
    geom: Geom,
    scale: f32,
    /// Header text while nothing is hovered: a file name or "N files selected".
    title: String,
    slices: Vec<gfx::Slice>,
    close: Mask,
    back: Mask,
    hover: Option<usize>,
    center_hover: bool,
}

struct Toast {
    hwnd: HWND,
    _surf: Surface,
    undo: bool,
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

/// Work area and scale of the monitor under `pt`.
fn monitor(pt: POINT) -> (RECT, f32) {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        (info.rcWork, x as f32 / 96.0)
    }
}

fn present(hwnd: HWND, surf: &Surface, at: POINT, alpha: u8) {
    unsafe {
        let size = SIZE { cx: surf.w, cy: surf.h };
        let src = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
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

/// Segoe icon-font code point for a slice.
fn icon_for(label: &str, id: &str) -> char {
    let label = label.trim_start_matches(['\u{2605}', ' ']);
    if id.starts_with("Send/Move to/") || id.starts_with("Send/Copy to/") {
        return if label.starts_with("Choose") { '\u{ED25}' } else { '\u{E8B7}' };
    }
    if id.starts_with("Mine/") {
        return '\u{E768}';
    }
    match label {
        "Convert" => '\u{E8AB}',
        "Resize" => '\u{E740}',
        "Rotate" | "90\u{b0} right" => '\u{E7AD}',
        "90\u{b0} left" => '\u{E7A7}',
        "180\u{b0}" => '\u{E777}',
        "Crop" => '\u{E7A8}',
        "PDF" | "To PDF" => '\u{EA90}',
        "Gray" => '\u{E793}',
        "More" => '\u{E712}',
        "Zip" | "Zip all" => '\u{F012}',
        "Unzip" | "Extract" => '\u{E8E5}',
        "Copy path" | "Copy name" | "Copy paths" | "Copy to" => '\u{E8C8}',
        "Reveal" => '\u{E838}',
        "SHA-256" => '\u{E928}',
        "Text" | "TXT" | "RTF" => '\u{E8D2}',
        "HTML" => '\u{E774}',
        "Word" | "DOCX" | "DOC" | "ODT" | "Office" => '\u{E8A5}',
        "Image" | "PNG" | "JPG" | "WEBP" | "BMP" | "ICO" | "GIF" | "TIFF" | "Slides" => '\u{EB9F}',
        "Split" => '\u{E8C6}',
        "Shrink" => '\u{E73F}',
        "Merge" => '\u{E8F4}',
        "Each" => '\u{E8FD}',
        "CSV" | "XLSX" | "XLS" | "ODS" => '\u{E80A}',
        "JSON" | "YAML" | "TOML" | "Pretty" | "Minify" => '\u{E943}',
        "Send" => '\u{E724}',
        "Move to" => '\u{E8DE}',
        "Rename" => '\u{E8AC}',
        "Mine" => '\u{E734}',
        "Fix" | "LF" | "CRLF" | "UTF-8" => '\u{E90F}',
        "Flip H" => '\u{E8AB}',
        "Flip V" => '\u{E8CB}',
        l if l.ends_with('%') => '\u{E740}',
        l if l.contains(':') => '\u{E7A8}',
        _ => '\u{E7C3}',
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

/// Window size and ring position for `count` slices at `scale`.
fn layout(count: usize, scale: f32) -> (i32, i32, Geom) {
    let r_out = if count > 8 { 152.0 } else { 132.0 } * scale;
    let (r_in, pad, head) = (42.0 * scale, 12.0 * scale, 40.0 * scale);
    // Wide enough for a long file name in the header.
    let w = (2.0 * (r_out + pad)).max(380.0 * scale).ceil() as i32;
    let h = (head + 2.0 * (r_out + pad)).ceil() as i32;
    let geom = Geom { cx: w as f32 / 2.0, cy: head + pad + r_out, r_in, r_out };
    (w, h, geom)
}

fn widest(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match &n.kind {
            Kind::Group(c) => widest(c),
            Kind::Run(_) => 0,
        })
        .max()
        .unwrap_or(0)
        .max(nodes.len())
}

impl View {
    fn new(scale: f32, geom: Geom, title: String, theme_name: &str) -> Self {
        let icons = Text::icons((19.0 * scale) as i32);
        let close = icons.mask("\u{E711}");
        let back = icons.mask("\u{E72B}");
        View {
            text: Text::new((12.5 * scale).round() as i32, 600),
            icons,
            palette: theme::palette(theme_name),
            geom,
            scale,
            title,
            slices: Vec::new(),
            close,
            back,
            hover: None,
            center_hover: false,
        }
    }

    fn load(&mut self, entries: &[Entry]) {
        self.slices = entries
            .iter()
            .map(|e| gfx::Slice {
                icon: self.icons.mask(&if e.app { '\u{E71D}' } else { icon_for(&e.label, &e.id) }.to_string()),
                label: self.text.fit(&e.label, (72.0 * self.scale) as i32),
            })
            .collect();
        self.hover = None;
        self.center_hover = false;
    }

    /// `zoom` shrinks the ring for the opening animation.
    fn paint(&self, surf: &mut Surface, entries: &[Entry], nested: bool, zoom: f32) {
        let header = match self.hover {
            Some(i) => entries[i].tip.as_str(),
            None if self.center_hover => {
                if nested {
                    "Back"
                } else {
                    "Close"
                }
            }
            None => self.title.as_str(),
        };
        let header = self.text.fit(header, surf.w - (40.0 * self.scale) as i32);
        let (w, h) = (surf.w, surf.h);
        let g = &self.geom;
        let ring = gfx::Ring {
            geom: Geom { cx: g.cx, cy: g.cy, r_in: g.r_in * zoom, r_out: g.r_out * zoom },
            scale: self.scale,
            palette: &self.palette,
            hover: self.hover,
            center_hover: self.center_hover,
            slices: &self.slices,
            center: if nested { &self.back } else { &self.close },
            header: &header,
        };
        gfx::draw_wheel(surf.pixels(), w, h, &ring);
    }
}

fn entries_of(nodes: &[Node]) -> Vec<Entry> {
    nodes
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
            app: n.app,
        })
        .collect()
}

/// Paints a wheel without showing it, for previews and screenshots.
/// `open` names a group to step into. Returns premultiplied BGRA and its size.
pub fn render(title: String, root: &[Node], theme_name: &str, scale: f32, open: &[&str], hover: Option<usize>) -> Option<(Vec<u32>, i32, i32)> {
    let mut trail = Vec::new();
    for name in open {
        trail.push(level(root, &trail).iter().position(|n| n.label == *name)?);
    }
    let (w, h, geom) = layout(widest(root), scale);
    let entries = entries_of(level(root, &trail));
    let mut view = View::new(scale, geom, title, theme_name);
    view.load(&entries);
    view.hover = hover.filter(|&i| i < entries.len());
    let mut surf = Surface::new(w, h)?;
    view.paint(&mut surf, &entries, !trail.is_empty(), 1.0);
    Some((surf.pixels().to_vec(), w, h))
}

impl Wheel {
    fn rebuild(&mut self) {
        self.entries = entries_of(level(&self.root, &self.trail));
        self.view.load(&self.entries);
    }

    fn repaint(&mut self) {
        let t = (self.frame as f32 / FRAMES as f32).min(1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        self.view.paint(&mut self.surf, &self.entries, !self.trail.is_empty(), 0.82 + 0.18 * ease);
        present(self.hwnd, &self.surf, self.origin, (60.0 + 195.0 * ease) as u8);
    }

    /// Slice and centre-button state at window coordinates (x, y).
    fn probe(&self, x: f32, y: f32, flick: bool) -> (Option<usize>, bool) {
        let g = &self.view.geom;
        let center = (x - g.cx).hypot(y - g.cy) < g.r_in;
        (gfx::slice_at(g, self.entries.len(), x, y, flick), center)
    }
}

/// Opens the wheel centred on `pt` for `path`. Does nothing if already open.
pub fn show(pt: POINT, title: String, key: String, path: PathBuf, root: Vec<Node>, settings: &Settings) {
    if root.is_empty() || WHEEL.with_borrow(|w| w.is_some()) {
        return;
    }
    let (area, scale) = monitor(pt);
    let (w, h, geom) = layout(widest(&root), scale);
    let origin = POINT {
        x: (pt.x - geom.cx as i32).clamp(area.left, (area.right - w).max(area.left)),
        y: (pt.y - geom.cy as i32).clamp(area.top, (area.bottom - h).max(area.top)),
    };
    // Keep the ring centred on the cursor unless the screen edge forces a shift.
    let r = geom.r_out;
    let geom = Geom {
        cx: ((pt.x - origin.x) as f32).clamp(r, w as f32 - r),
        cy: ((pt.y - origin.y) as f32).clamp(r, h as f32 - r),
        ..geom
    };

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
        view: View::new(scale, geom, title, &settings.theme),
        origin,
        path,
        key,
        root,
        trail: Vec::new(),
        entries: Vec::new(),
        flick: settings.flick(),
        frame: if settings.animate { 0 } else { FRAMES },
        left_center: false,
    };
    wheel.rebuild();
    wheel.repaint();
    // Forget an Escape from before the wheel existed.
    let _ = escape_pressed();
    WHEEL.with_borrow_mut(|s| *s = Some(wheel));
    hook::WHEEL_OPEN.store(true, std::sync::atomic::Ordering::Release);
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
    hook::WHEEL_OPEN.store(false, std::sync::atomic::Ordering::Release);
    // Don't reopen on the same file while the key is still held.
    hook::block_until_released();
    unsafe {
        let _ = KillTimer(Some(wheel.hwnd), TIMER_POLL);
        let _ = DestroyWindow(wheel.hwnd);
    }
    drop(wheel);
    hook::trim_later();
}

enum Outcome {
    Stay,
    Close,
    Pick(usize),
}

/// Held now, or tapped since the last look (a quick tap can fall between polls).
fn escape_pressed() -> bool {
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) != 0 }
}

fn tick() -> Outcome {
    WHEEL.with_borrow_mut(|s| {
        let Some(w) = s.as_mut() else { return Outcome::Stay };
        if escape_pressed() {
            return Outcome::Close;
        }
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let (hover, center) = w.probe((pt.x - w.origin.x) as f32, (pt.y - w.origin.y) as f32, w.flick);
        w.left_center |= !center;
        let center = center && w.left_center;
        let animating = w.frame < FRAMES;
        if animating {
            w.frame += 1;
        }
        if animating || hover != w.view.hover || center != w.view.center_hover {
            w.view.hover = hover;
            w.view.center_hover = center;
            w.repaint();
        }
        if w.flick && !hook::trigger_held() {
            return hover.map_or(Outcome::Close, Outcome::Pick);
        }
        Outcome::Stay
    })
}

/// Runs the chosen slice: opens a sub-wheel or executes an action.
fn pick(i: usize) {
    enum Next {
        Done,
        Run(Runner, PathBuf, String, String, String, bool),
    }
    let next = WHEEL.with_borrow_mut(|s| {
        let w = s.as_mut()?;
        let entry = w.entries.get(i)?;
        match &entry.act {
            Act::Run(r) => {
                let label = entry.tip.trim_end_matches(" (your usual)").to_string();
                Some(Next::Run(r.clone(), w.path.clone(), w.key.clone(), entry.id.clone(), label, entry.slow))
            }
            Act::Open(idx) => {
                w.trail.push(*idx);
                // A group needs a second look at the pointer, so stop waiting for the key release.
                w.flick = false;
                w.rebuild();
                w.repaint();
                Some(Next::Done)
            }
        }
    });
    if let Some(Next::Run(runner, path, key, id, label, slow)) = next {
        close();
        // "Choose folder" and rename open their own windows; don't promote them.
        if !id.starts_with("Send/") {
            recent::record(&key, &id);
        }
        if slow {
            toast_info("Working on it\u{2026}");
        }
        std::thread::spawn(move || {
            let (result, undoable) = actions::execute(&runner, &path, &label);
            match result {
                Ok(msg) => hook::post_result(true, msg, undoable),
                Err(msg) => hook::post_result(false, msg, undoable),
            }
        });
    }
}

/// Centre button: up one level, or close.
fn back() {
    let closing = WHEEL.with_borrow_mut(|s| {
        let Some(w) = s.as_mut() else { return false };
        if w.trail.pop().is_none() {
            return true;
        }
        w.rebuild();
        w.repaint();
        false
    });
    if closing {
        close();
    }
}

/// A mouse press anywhere that the wheel itself didn't claim dismisses it.
pub fn on_outside_click(pt: POINT) {
    let claimed = WHEEL.with_borrow(|s| {
        s.as_ref().is_some_and(|w| {
            let (slice, center) = w.probe((pt.x - w.origin.x) as f32, (pt.y - w.origin.y) as f32, false);
            slice.is_some() || center
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
            match WHEEL.with_borrow(|s| s.as_ref().map(|w| w.probe(x, y, false))) {
                Some((Some(i), _)) => pick(i),
                Some((None, true)) => back(),
                _ => {}
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

/// The result of an action. With `undo`, the toast carries a button for it.
pub fn toast(msg: &str, ok: bool, undo: bool, theme_name: &str) {
    if msg.is_empty() {
        close_toast();
        return;
    }
    let color = if ok { gfx::OK } else { gfx::FAIL };
    show_toast(msg, Some(color), undo, theme_name, if undo { UNDO_MS } else { TOAST_MS });
}

/// A neutral "still going" toast that stays until the result replaces it.
pub fn toast_info(msg: &str) {
    show_toast(msg, None, false, &crate::config::load().theme, 5 * 60 * 1000);
}

/// Draws the toast pill; returns it with its size.
pub fn paint_toast(msg: &str, color: Option<gfx::Rgb>, undo: bool, theme_name: &str, scale: f32, max_w: f32) -> Option<(Surface, i32, i32)> {
    let p = theme::palette(theme_name);
    let text = Text::new((14.0 * scale) as i32, 600);
    let button = undo.then(|| text.mask("Undo"));
    let button_w = button.as_ref().map_or(0.0, |b| b.w as f32 + 24.0 * scale);
    let label = text.fit(msg, (max_w - button_w - 60.0 * scale) as i32);
    let dot = 8.0 * scale;
    let lead = if color.is_some() { dot + 10.0 * scale } else { 0.0 };
    let trail = if undo { button_w + 8.0 * scale } else { 18.0 * scale };
    let (pw, ph) = (18.0 * scale + lead + label.w as f32 + trail + if undo { 10.0 * scale } else { 0.0 }, 40.0 * scale);
    let (w, h) = (pw.ceil() as i32 + 4, ph.ceil() as i32 + 4);
    let mut surf = Surface::new(w, h)?;
    let px = surf.pixels();
    px.fill(0);
    gfx::capsule(px, w, h, (2.0, 2.0, pw, ph), p.base, Some((p.rim, p.rim_alpha)));
    let mid = 2.0 + ph / 2.0;
    let mut x = 2.0 + 18.0 * scale;
    if let Some(color) = color {
        gfx::capsule(px, w, h, (x, mid - dot / 2.0, dot, dot), color, None);
        x += lead;
    }
    gfx::text(px, w, h, &label, x + label.w as f32 / 2.0, mid, p.text);
    if let Some(button) = button {
        let rect = (2.0 + pw - button_w - 6.0 * scale, 2.0 + 6.0 * scale, button_w, ph - 12.0 * scale);
        gfx::capsule(px, w, h, rect, p.hover, None);
        gfx::text(px, w, h, &button, rect.0 + rect.2 / 2.0, mid, p.hover_text);
    }
    Some((surf, w, h))
}

fn show_toast(msg: &str, color: Option<gfx::Rgb>, undo: bool, theme_name: &str, ms: u32) {
    close_toast();
    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let (area, scale) = monitor(cursor);
    let max_w = ((area.right - area.left) as f32 * 0.6).max(360.0);
    let Some((surf, w, h)) = paint_toast(msg, color, undo, theme_name, scale, max_w) else { return };
    let at = POINT { x: (area.left + area.right - w) / 2, y: area.bottom - h - (60.0 * scale) as i32 };
    let mut style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
    if !undo {
        // Purely informational toasts never get in the way of a click.
        style |= WS_EX_TRANSPARENT;
    }
    let hwnd = unsafe {
        CreateWindowExW(style, w!("RadialActionsToast"), w!(""), WS_POPUP, at.x, at.y, w, h, None, None, None, None)
    };
    let Ok(hwnd) = hwnd else { return };
    present(hwnd, &surf, at, 255);
    TOAST.with_borrow_mut(|t| *t = Some(Toast { hwnd, _surf: surf, undo }));
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
        hook::trim_later();
    }
}

/// Reverses the last action off the UI thread and reports back.
pub fn undo_last() {
    std::thread::spawn(|| match journal::undo() {
        Ok(msg) => hook::post_result(true, msg, false),
        Err(msg) => hook::post_result(false, msg, false),
    });
}

unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_TOAST => {
            close_toast();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if TOAST.with_borrow(|t| t.as_ref().is_some_and(|t| t.undo)) {
                close_toast();
                undo_last();
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
