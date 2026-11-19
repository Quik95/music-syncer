//! Nord palette in OKLCH. Surfaces are the background raised in lightness, so equal steps
//! look equally brighter.

use std::sync::LazyLock;

use gpui::Rgba;
use palette::{FromColor as _, OklabHue, Oklch, Srgb};

const fn oklch(l: f32, chroma: f32, hue: f32) -> Oklch {
    Oklch::new_const(l, chroma, OklabHue::new(hue))
}

/// `color` with lightness raised by `by` (OKLCH L is 0–1). Chroma grows in proportion,
/// as in Nord's own greys; with fixed chroma lighter shades turn grey.
const fn lighter(color: Oklch, by: f32) -> Oklch {
    let l = color.l + by;
    Oklch::new_const(l, color.chroma * l / color.l, color.hue)
}

// Base colors (Nord name in parentheses).
const POLAR_NIGHT: Oklch = oklch(0.324, 0.023, 264.); // nord0
const SNOW_STORM: Oklch = oklch(0.951, 0.007, 261.); // nord6
const FROST_TEAL: Oklch = oklch(0.775, 0.062, 217.5); // nord8
const FROST_BLUE: Oklch = oklch(0.697, 0.059, 249.); // nord9
const AURORA_RED: Oklch = oklch(0.606, 0.121, 15.); // nord11

// Lightness steps above the background.
const RAISED: f32 = 0.055;
const HIGHLIGHT: f32 = 0.09;
const OUTLINE: f32 = 0.13;

fn rgba(color: Oklch) -> Rgba {
    let rgb = Srgb::from_color(color);
    Rgba {
        r: rgb.red.clamp(0., 1.),
        g: rgb.green.clamp(0., 1.),
        b: rgb.blue.clamp(0., 1.),
        a: 1.,
    }
}

/// Each color is converted to sRGB once, on first use.
macro_rules! colors {
    ($($name:ident = $color:expr;)*) => {
        $(pub static $name: LazyLock<Rgba> = LazyLock::new(|| rgba($color));)*
    };
}

colors! {
    BACKGROUND = POLAR_NIGHT;
    HEADER_BACKGROUND = lighter(POLAR_NIGHT, RAISED);
    HEADER_HOVER = lighter(POLAR_NIGHT, HIGHLIGHT);
    BORDER = lighter(POLAR_NIGHT, OUTLINE);
    TEXT = SNOW_STORM;
    TEXT_MUTED = FROST_BLUE;
    DIRECTORY = FROST_TEAL;
    ERROR = AURORA_RED;
    ROW_HOVER = lighter(POLAR_NIGHT, RAISED);
    ROW_ACTIVE = lighter(POLAR_NIGHT, OUTLINE);
    ROW_ACTIVE_UNFOCUSED = lighter(POLAR_NIGHT, HIGHLIGHT);
    FOCUS = FROST_TEAL;
    SCROLLBAR_TRACK = lighter(POLAR_NIGHT, RAISED);
    SCROLLBAR_THUMB = lighter(POLAR_NIGHT, 0.21);
    SCROLLBAR_THUMB_ACTIVE = lighter(POLAR_NIGHT, 0.30);
}
