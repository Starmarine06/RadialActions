//! Software drawing for the wheel and toast: anti-aliased shapes written
//! straight into a premultiplied BGRA bitmap, with GDI used only to rasterise
//! text into a coverage mask.

use std::f32::consts::{FRAC_PI_2, TAU};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::*;

/// A 32-bit top-down bitmap selected into a memory DC.
pub struct Surface {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
}

fn dib(dc: HDC, w: i32, h: i32) -> Option<(HBITMAP, *mut u32)> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bmp = unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    (!bits.is_null()).then_some((bmp, bits as *mut u32))
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let Some((bmp, bits)) = dib(dc, w, h) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bmp.into());
            Some(Self { w, h, dc, bmp, old, bits })
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Grayscale text coverage, 0..=255 per pixel.
pub struct Mask {
    pub w: i32,
    pub h: i32,
    pub a: Vec<u8>,
}

/// A font selected into a scratch DC, used to rasterise labels.
pub struct Text {
    dc: HDC,
    font: HFONT,
    old: HGDIOBJ,
}

impl Text {
    pub fn new(px: i32, weight: i32) -> Self {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let font = CreateFontW(
                -px,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let old = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            Self { dc, font, old }
        }
    }

    pub fn mask(&self, text: &str) -> Mask {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let empty = Mask { w: 0, h: 0, a: Vec::new() };
        if wide.is_empty() {
            return empty;
        }
        unsafe {
            let mut size = SIZE::default();
            if !GetTextExtentPoint32W(self.dc, &wide, &mut size).as_bool() {
                return empty;
            }
            let (w, h) = (size.cx + 2, size.cy + 2);
            let Some((bmp, bits)) = dib(self.dc, w, h) else { return empty };
            let prev = SelectObject(self.dc, bmp.into());
            let _ = TextOutW(self.dc, 1, 1, &wide);
            let _ = GdiFlush();
            let px = std::slice::from_raw_parts(bits, (w * h) as usize);
            let a = px.iter().map(|p| (p & 0xFF) as u8).collect();
            SelectObject(self.dc, prev);
            let _ = DeleteObject(bmp.into());
            Mask { w, h, a }
        }
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.font.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub type Rgb = (f32, f32, f32);

pub const BASE: Rgb = (40.0, 43.0, 58.0);
pub const ACCENT: Rgb = (88.0, 101.0, 242.0);
pub const OK: Rgb = (30.0, 120.0, 80.0);
pub const FAIL: Rgb = (160.0, 48.0, 56.0);
pub const INFO: Rgb = (52.0, 74.0, 128.0);
pub const WHITE: Rgb = (255.0, 255.0, 255.0);

/// Source-over of a straight-alpha colour onto a premultiplied pixel.
#[inline]
fn over(dst: &mut u32, (r, g, b): Rgb, a: f32) {
    if a <= 0.0 {
        return;
    }
    let d = *dst;
    let inv = 1.0 - a;
    let ch = |shift: u32, c: f32| (c * a + ((d >> shift) & 0xFF) as f32 * inv + 0.5).min(255.0) as u32;
    let oa = (a * 255.0 + (d >> 24) as f32 * inv + 0.5).min(255.0) as u32;
    *dst = (oa << 24) | (ch(16, r) << 16) | (ch(8, g) << 8) | ch(0, b);
}

pub struct Geom {
    pub cx: f32,
    pub cy: f32,
    pub r_in: f32,
    pub r_out: f32,
}

/// Which of `n` slices lies at (x, y). In flick mode the distance beyond the
/// ring doesn't matter, only the direction.
pub fn slice_at(g: &Geom, n: usize, x: f32, y: f32, flick: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (dx, dy) = (x - g.cx, y - g.cy);
    let r = dx.hypot(dy);
    if r < g.r_in || (!flick && r > g.r_out) {
        return None;
    }
    let sector = TAU / n as f32;
    let a = (dy.atan2(dx) + FRAC_PI_2 + sector / 2.0).rem_euclid(TAU);
    Some(((a / sector) as usize).min(n - 1))
}

fn blit(px: &mut [u32], w: i32, h: i32, m: &Mask, cx: f32, cy: f32, color: Rgb, alpha: f32) {
    let x0 = (cx - m.w as f32 / 2.0).round() as i32;
    let y0 = (cy - m.h as f32 / 2.0).round() as i32;
    for my in 0..m.h {
        for mx in 0..m.w {
            let (x, y) = (x0 + mx, y0 + my);
            if x < 0 || y < 0 || x >= w || y >= h {
                continue;
            }
            let c = m.a[(my * m.w + mx) as usize] as f32 / 255.0;
            over(&mut px[(y * w + x) as usize], color, c * alpha);
        }
    }
}

/// Rounded rectangle with a text label centred in it.
pub fn pill(px: &mut [u32], w: i32, h: i32, rect: (f32, f32, f32, f32), fill: Rgb, label: &Mask) {
    let (x0, y0, pw, ph) = rect;
    let (hx, hy) = (pw / 2.0, ph / 2.0);
    let rad = hy;
    let (mx, my) = (x0 + hx, y0 + hy);
    for y in (y0.floor() as i32).max(0)..((y0 + ph).ceil() as i32).min(h) {
        for x in (x0.floor() as i32).max(0)..((x0 + pw).ceil() as i32).min(w) {
            let (qx, qy) = ((x as f32 + 0.5 - mx).abs() - (hx - rad), (y as f32 + 0.5 - my).abs() - (hy - rad));
            let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad;
            over(&mut px[(y * w + x) as usize], fill, (0.5 - dist).clamp(0.0, 1.0) * 0.97);
        }
    }
    blit(px, w, h, label, mx, my, WHITE, 1.0);
}

/// Draws the ring of slices, their labels and the header pill.
pub fn draw_wheel(
    px: &mut [u32],
    w: i32,
    h: i32,
    g: &Geom,
    scale: f32,
    hover: Option<usize>,
    labels: &[Mask],
    header: &Mask,
) {
    px.fill(0);
    let n = labels.len();
    if n == 0 {
        return;
    }
    let sector = TAU / n as f32;
    let gap_half = 1.5 * scale;

    let (x_lo, x_hi) = ((g.cx - g.r_out - 2.0).max(0.0) as i32, ((g.cx + g.r_out + 2.0) as i32).min(w));
    let (y_lo, y_hi) = ((g.cy - g.r_out - 2.0).max(0.0) as i32, ((g.cy + g.r_out + 2.0) as i32).min(h));
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let (dx, dy) = (x as f32 + 0.5 - g.cx, y as f32 + 0.5 - g.cy);
            let r = dx.hypot(dy);
            let radial = (r - g.r_in + 0.5).clamp(0.0, 1.0) * (g.r_out - r + 0.5).clamp(0.0, 1.0);
            if radial <= 0.0 {
                continue;
            }
            let a = (dy.atan2(dx) + FRAC_PI_2 + sector / 2.0).rem_euclid(TAU);
            let idx = ((a / sector) as usize).min(n - 1);
            let edge = if n > 1 {
                let into = a - idx as f32 * sector;
                into.min(sector - into) * r
            } else {
                f32::MAX
            };
            let cov = radial * (edge - gap_half + 0.5).clamp(0.0, 1.0);
            let (color, alpha) = if hover == Some(idx) { (ACCENT, 0.98) } else { (BASE, 0.97) };
            let dst = &mut px[(y * w + x) as usize];
            over(dst, color, cov * alpha);
            // Thin light rim on the inner and outer edges keeps the ring readable on dark windows.
            let rim = ((1.3 * scale - (r - g.r_in)).min(1.3 * scale - (g.r_out - r)) / scale).clamp(0.0, 1.0);
            over(dst, WHITE, cov * rim * 0.28);
        }
    }

    let mid = (g.r_in + g.r_out) / 2.0;
    for (i, label) in labels.iter().enumerate() {
        let ang = -FRAC_PI_2 + i as f32 * sector;
        blit(px, w, h, label, g.cx + mid * ang.cos(), g.cy + mid * ang.sin(), WHITE, 1.0);
    }

    if header.w > 0 {
        let (pw, ph) = (header.w as f32 + 26.0 * scale, header.h as f32 + 10.0 * scale);
        pill(px, w, h, ((w as f32 - pw) / 2.0, 4.0 * scale, pw, ph), BASE, header);
    }
}

/// Tray icon: a six-slice ring with the top slice lit, as straight-alpha BGRA.
pub fn draw_icon(px: &mut [u32], size: i32) {
    px.fill(0);
    let s = size as f32;
    let (c, r_in, r_out) = (s / 2.0, s * 0.24, s * 0.48);
    let sector = TAU / 6.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let r = dx.hypot(dy);
            let radial = (r - r_in + 0.5).clamp(0.0, 1.0) * (r_out - r + 0.5).clamp(0.0, 1.0);
            if radial <= 0.0 {
                continue;
            }
            let a = (dy.atan2(dx) + FRAC_PI_2 + sector / 2.0).rem_euclid(TAU);
            let idx = ((a / sector) as usize).min(5);
            let into = a - idx as f32 * sector;
            let cov = radial * (into.min(sector - into) * r - s * 0.03 + 0.5).clamp(0.0, 1.0);
            let color = if idx == 0 { WHITE } else { ACCENT };
            over(&mut px[(y * size + x) as usize], color, cov);
        }
    }
    // Icons want straight alpha, not premultiplied.
    for p in px.iter_mut() {
        let a = *p >> 24;
        if a > 0 && a < 255 {
            let un = |sh: u32| ((((*p >> sh) & 0xFF) * 255 + a / 2) / a).min(255);
            *p = (a << 24) | (un(16) << 16) | (un(8) << 8) | un(0);
        }
    }
}

impl Surface {
    pub fn bitmap(&self) -> HBITMAP {
        self.bmp
    }
}
