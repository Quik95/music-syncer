//! Nord palette in OKLCH. Surfaces are the background raised in lightness, so equal steps
//! look equally brighter. [`apply`] dresses the kit's dark theme in it; the statics are the
//! colors the kit has no slot for.

use std::sync::LazyLock;

use gpui_kit::{
    App, Rgba,
    component::{Theme, ThemeMode},
};
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
const AURORA_YELLOW: Oklch = oklch(0.855, 0.089, 84.); // nord13
const AURORA_GREEN: Oklch = oklch(0.768, 0.075, 131.); // nord14

// Lightness steps above the background.
const RAISED: f32 = 0.055;
const HIGHLIGHT: f32 = 0.09;
const OUTLINE: f32 = 0.13;

/// Dark, low-chroma shade of `color` for backgrounds behind normal text.
const fn tint(color: Oklch, l: f32) -> Oklch {
    Oklch::new_const(l, 0.045, color.hue)
}

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
    DIRECTORY = FROST_TEAL;
    ROW_ACTIVE_UNFOCUSED = lighter(POLAR_NIGHT, HIGHLIGHT);
    DIFFERS = AURORA_YELLOW;
    ONLY_HERE = tint(AURORA_GREEN, 0.40);
    MISSING_HERE = tint(AURORA_RED, 0.36);
    CHANGED = tint(AURORA_YELLOW, 0.40);
}

/// Switches the kit to its dark theme with the Nord colors. Call once, after
/// `gpui_kit::init`.
pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        let background = rgba(POLAR_NIGHT).into();
        let raised = rgba(lighter(POLAR_NIGHT, RAISED)).into();
        let highlight = rgba(lighter(POLAR_NIGHT, HIGHLIGHT)).into();
        let outline = rgba(lighter(POLAR_NIGHT, OUTLINE)).into();
        let text = rgba(SNOW_STORM).into();
        let muted_text = rgba(FROST_BLUE).into();
        let focus = rgba(FROST_TEAL).into();
        let pressed = rgba(tint(FROST_TEAL, 0.48)).into();
        let pressed_hover = rgba(tint(FROST_TEAL, 0.54)).into();
        let red = rgba(AURORA_RED).into();
        let yellow = rgba(AURORA_YELLOW).into();
        let green = rgba(AURORA_GREEN).into();

        let colors = &mut theme.colors;
        colors.background = background;
        colors.foreground = text;
        colors.muted = raised;
        colors.muted_foreground = muted_text;
        colors.border = outline;
        colors.input = outline;
        colors.ring = focus;
        colors.caret = text;
        colors.selection = pressed;
        colors.accent = outline;
        colors.accent_foreground = text;
        colors.popover = raised;
        colors.popover_foreground = text;
        colors.group_box = raised;
        colors.group_box_foreground = text;

        colors.list = background;
        colors.list_even = background;
        colors.list_head = raised;
        colors.list_hover = raised;
        colors.list_active = outline;
        colors.list_active_border = focus;

        colors.primary = pressed;
        colors.primary_hover = pressed_hover;
        colors.primary_active = pressed;
        colors.primary_foreground = text;
        colors.secondary = highlight;
        colors.secondary_hover = outline;
        colors.secondary_active = pressed;
        colors.secondary_foreground = text;
        colors.button = highlight;
        colors.button_hover = outline;
        colors.button_active = pressed;
        colors.button_foreground = text;
        colors.button_primary = pressed;
        colors.button_primary_hover = pressed_hover;
        colors.button_primary_active = pressed;
        colors.button_primary_foreground = text;
        colors.button_secondary = highlight;
        colors.button_secondary_hover = outline;
        colors.button_secondary_active = pressed;
        colors.button_secondary_foreground = text;

        colors.danger = red;
        colors.danger_foreground = text;
        colors.warning = yellow;
        colors.success = green;
        colors.info = muted_text;
        colors.progress_bar = muted_text;

        // The scrollbar overlays the rows' right edge; an opaque track would cut into them.
        colors.scrollbar = theme.transparent;
        colors.scrollbar_thumb = rgba(lighter(POLAR_NIGHT, 0.21)).into();
        colors.scrollbar_thumb_hover = rgba(lighter(POLAR_NIGHT, 0.30)).into();

        colors.status_bar = raised;
        colors.status_bar_border = outline;
        colors.title_bar = raised;
        colors.title_bar_border = outline;
    });
}
