//! Colours for the wheel and toasts: Windows light/dark with the user's accent,
//! or neon.

use crate::gfx::Rgb;
use crate::sys;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub base: Rgb,
    pub hover: Rgb,
    pub text: Rgb,
    pub hover_text: Rgb,
    pub rim: Rgb,
    pub rim_alpha: f32,
    /// Soft halo outside the ring: a shadow, or a coloured glow for neon.
    pub halo: Rgb,
    pub halo_alpha: f32,
    pub glow: bool,
}

const WHITE: Rgb = (255.0, 255.0, 255.0);
const INK: Rgb = (28.0, 28.0, 30.0);

/// Windows' default blue, for when no accent can be read.
const BLUE: Rgb = (0.0, 103.0, 192.0);

fn luminance((r, g, b): Rgb) -> f32 {
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}

pub fn accent() -> Rgb {
    // Stored as 0xAABBGGRR.
    sys::read_dword("Software\\Microsoft\\Windows\\DWM", "AccentColor")
        .map(|v| ((v & 0xFF) as f32, ((v >> 8) & 0xFF) as f32, ((v >> 16) & 0xFF) as f32))
        .unwrap_or(BLUE)
}

pub fn system_is_light() -> bool {
    sys::read_dword("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize", "AppsUseLightTheme")
        .is_none_or(|v| v != 0)
}

pub fn palette(name: &str) -> Palette {
    if name == "neon" {
        return Palette {
            base: (14.0, 10.0, 30.0),
            hover: (255.0, 42.0, 157.0),
            text: (214.0, 250.0, 255.0),
            hover_text: WHITE,
            rim: (0.0, 234.0, 255.0),
            rim_alpha: 0.95,
            halo: (0.0, 214.0, 255.0),
            halo_alpha: 0.55,
            glow: true,
        };
    }
    let light = match name {
        "light" => true,
        "dark" => false,
        _ => system_is_light(),
    };
    let accent = accent();
    Palette {
        base: if light { (249.0, 249.0, 250.0) } else { (44.0, 44.0, 46.0) },
        hover: accent,
        text: if light { INK } else { WHITE },
        hover_text: if luminance(accent) > 0.6 { INK } else { WHITE },
        rim: if light { (0.0, 0.0, 0.0) } else { WHITE },
        rim_alpha: if light { 0.16 } else { 0.22 },
        halo: (0.0, 0.0, 0.0),
        halo_alpha: if light { 0.22 } else { 0.38 },
        glow: false,
    }
}
