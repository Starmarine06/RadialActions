//! Software drawing for the wheel and toast: anti-aliased shapes written
//! straight into a premultiplied BGRA bitmap, with GDI used only to rasterise
//! text into a coverage mask.

use crate::theme::Palette;
use std::f32::consts::{FRAC_PI_2, TAU};
use windows::core::PCWSTR;
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
        Self::with_face(px, weight, "Segoe UI")
    }

    /// The Windows icon font: Fluent on 11, MDL2 on 10 (same code points).
    pub fn icons(px: i32) -> Self {
        let fluent = Self::with_face(px, 400, "Segoe Fluent Icons");
        if fluent.face() == "Segoe Fluent Icons" {
            fluent
        } else {
            Self::with_face(px, 400, "Segoe MDL2 Assets")
        }
    }

    fn face(&self) -> String {
        let mut buf = [0u16; 64];
        let n = unsafe { GetTextFaceW(self.dc, Some(&mut buf)) } as usize;
        String::from_utf16_lossy(&buf[..n.saturating_sub(1)])
    }

    /// Like `mask`, shortened with an ellipsis until it is at most `max` wide.
    pub fn fit(&self, text: &str, max: i32) -> Mask {
        let full = self.mask(text);
        if full.w <= max {
            return full;
        }
        let chars: Vec<char> = text.chars().collect();
        // Trim from the middle so a file's extension stays readable.
        let (mut lo, mut hi) = (1usize, chars.len().saturating_sub(1).max(1));
        while lo < hi {
            let keep = (lo + hi).div_ceil(2);
            if self.mask(&middle(&chars, keep)).w <= max {
                lo = keep;
            } else {
                hi = keep - 1;
            }
        }
        self.mask(&middle(&chars, lo))
    }

    pub fn with_face(px: i32, weight: i32, face: &str) -> Self {
        let face: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
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
                PCWSTR(face.as_ptr()),
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

fn middle(chars: &[char], keep: usize) -> String {
    let head = keep.div_ceil(2);
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(&chars[chars.len() - (keep - head)..]);
    out
}

pub type Rgb = (f32, f32, f32);

pub const OK: Rgb = (46.0, 190.0, 120.0);
pub const FAIL: Rgb = (232.0, 80.0, 90.0);
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

/// Signed distance to a rounded rectangle, negative inside.
fn rounded(x: f32, y: f32, (x0, y0, w, h): (f32, f32, f32, f32), rad: f32) -> f32 {
    let (hx, hy) = (w / 2.0, h / 2.0);
    let (qx, qy) = ((x - x0 - hx).abs() - (hx - rad), (y - y0 - hy).abs() - (hy - rad));
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
}

/// A filled capsule with an optional thin outline.
pub fn capsule(px: &mut [u32], w: i32, h: i32, rect: (f32, f32, f32, f32), fill: Rgb, rim: Option<(Rgb, f32)>) {
    let (x0, y0, pw, ph) = rect;
    for y in (y0.floor() as i32).max(0)..((y0 + ph).ceil() as i32).min(h) {
        for x in (x0.floor() as i32).max(0)..((x0 + pw).ceil() as i32).min(w) {
            let dist = rounded(x as f32 + 0.5, y as f32 + 0.5, rect, ph / 2.0);
            let cov = (0.5 - dist).clamp(0.0, 1.0);
            let dst = &mut px[(y * w + x) as usize];
            over(dst, fill, cov * 0.98);
            if let Some((color, alpha)) = rim {
                let edge = (1.6 + dist).clamp(0.0, 1.0);
                over(dst, color, cov * edge * alpha);
            }
        }
    }
}

/// Text centred on (cx, cy).
pub fn text(px: &mut [u32], w: i32, h: i32, m: &Mask, cx: f32, cy: f32, color: Rgb) {
    blit(px, w, h, m, cx, cy, color, 1.0);
}

/// What one slice shows.
pub struct Slice {
    pub icon: Mask,
    pub label: Mask,
}

pub struct Ring<'a> {
    pub geom: Geom,
    pub scale: f32,
    pub palette: &'a Palette,
    pub hover: Option<usize>,
    pub center_hover: bool,
    pub slices: &'a [Slice],
    /// Glyph in the middle: close, or back inside a group.
    pub center: &'a Mask,
    pub header: &'a Mask,
}

/// Draws the ring of slices, the centre button and the header pill.
pub fn draw_wheel(px: &mut [u32], w: i32, h: i32, ring: &Ring) {
    px.fill(0);
    let g = &ring.geom;
    let (scale, p) = (ring.scale, ring.palette);
    let n = ring.slices.len();
    if n == 0 {
        return;
    }
    let sector = TAU / n as f32;
    let gap_half = 1.5 * scale;
    let halo_w = 9.0 * scale;
    let center_r = g.r_in - 6.0 * scale;

    let reach = g.r_out + halo_w + 2.0;
    let (x_lo, x_hi) = ((g.cx - reach).max(0.0) as i32, ((g.cx + reach) as i32).min(w));
    let (y_lo, y_hi) = ((g.cy - reach).max(0.0) as i32, ((g.cy + reach) as i32).min(h));
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let (dx, dy) = (x as f32 + 0.5 - g.cx, y as f32 + 0.5 - g.cy);
            let r = dx.hypot(dy);
            let dst = &mut px[(y * w + x) as usize];

            // Halo first, so the ring covers its inner edge.
            if r > g.r_out - 1.0 && r < g.r_out + halo_w {
                let fade = 1.0 - ((r - g.r_out) / halo_w).clamp(0.0, 1.0);
                over(dst, p.halo, fade * fade * p.halo_alpha);
            }

            // Centre button.
            let disc = (center_r - r + 0.5).clamp(0.0, 1.0);
            if disc > 0.0 {
                let color = if ring.center_hover { p.hover } else { p.base };
                over(dst, color, disc * 0.97);
                let edge = (1.4 * scale - (center_r - r)).clamp(0.0, 1.0);
                over(dst, p.rim, disc * edge * p.rim_alpha);
                continue;
            }

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
            let hot = ring.hover == Some(idx);
            over(dst, if hot { p.hover } else { p.base }, cov * 0.97);
            // A rim keeps the ring readable on any window; neon outlines each slice.
            let mut rim = ((1.3 * scale - (r - g.r_in)).max(1.3 * scale - (g.r_out - r)) / scale).clamp(0.0, 1.0);
            if p.glow {
                rim = rim.max((1.2 * scale - (edge - gap_half)).clamp(0.0, 1.0));
            }
            over(dst, p.rim, cov * rim * p.rim_alpha);
        }
    }

    let mid = (g.r_in + g.r_out) / 2.0;
    for (i, slice) in ring.slices.iter().enumerate() {
        let ang = -FRAC_PI_2 + i as f32 * sector;
        let (cx, cy) = (g.cx + mid * ang.cos(), g.cy + mid * ang.sin());
        let color = if ring.hover == Some(i) { p.hover_text } else { p.text };
        blit(px, w, h, &slice.icon, cx, cy - 9.0 * scale, color, 1.0);
        blit(px, w, h, &slice.label, cx, cy + 11.0 * scale, color, 1.0);
    }

    let color = if ring.center_hover { p.hover_text } else { p.text };
    blit(px, w, h, ring.center, g.cx, g.cy, color, 1.0);

    let header = ring.header;
    if header.w > 0 {
        let (pw, ph) = (header.w as f32 + 26.0 * scale, header.h as f32 + 10.0 * scale);
        let rect = ((w as f32 - pw) / 2.0, 4.0 * scale, pw, ph);
        capsule(px, w, h, rect, p.base, Some((p.rim, p.rim_alpha)));
        blit(px, w, h, header, rect.0 + pw / 2.0, rect.1 + ph / 2.0, p.text, 1.0);
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
            let color = if idx == 0 { WHITE } else { (47.0, 129.0, 247.0) };
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
