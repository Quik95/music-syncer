//! Small building blocks shared by views.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, Div, ElementId, Rgba, Stateful, div, ease_in_out, prelude::*, px,
    relative,
};

use super::theme;

/// Width of the sweeping highlight as a share of the filled part.
const SWEEP_WIDTH: f32 = 0.4;

/// A clickable label. A `pressed` one stays highlighted, for toggles that are on; a
/// disabled one is dimmed and ignores the pointer, though click handlers still run.
pub fn button(id: impl Into<ElementId>, pressed: bool, enabled: bool) -> Stateful<Div> {
    let button = div().id(id).px_2().rounded_sm();
    if !enabled {
        return button.bg(*theme::BUTTON).text_color(*theme::TEXT_MUTED);
    }
    let background = if pressed {
        *theme::BUTTON_PRESSED
    } else {
        *theme::BUTTON
    };
    button
        .cursor_pointer()
        .bg(background)
        .hover(|style| style.bg(*theme::BUTTON_HOVER))
}

/// A bar filled to `fraction` (0–1), with a highlight sweeping across the filled part so
/// it shows activity even when the fraction stalls.
pub fn progress_bar(fraction: f32) -> impl IntoElement {
    let sweep = div()
        .absolute()
        .top_0()
        .h_full()
        .w(relative(SWEEP_WIDTH))
        .bg(*theme::PROGRESS_SWEEP)
        .with_animation(
            "progress-sweep",
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_easing(ease_in_out),
            // From fully left of the fill to fully right of it.
            |sweep, delta| sweep.left(relative(delta * (1. + SWEEP_WIDTH) - SWEEP_WIDTH)),
        );
    let fill = div()
        .relative()
        .h_full()
        .w(relative(fraction.clamp(0., 1.)))
        .overflow_hidden()
        .rounded_full()
        .bg(*theme::PROGRESS)
        .child(sweep);
    div()
        .w(px(320.))
        .h(px(6.))
        .rounded_full()
        .bg(*theme::PROGRESS_TRACK)
        .child(fill)
}

/// A dimmed layer over the whole parent, which must be `relative`, with `card` centered on
/// it. Swallows pointer events so the content below cannot be used meanwhile.
pub fn modal(card: impl IntoElement) -> Div {
    div()
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(Rgba {
            r: 0.,
            g: 0.,
            b: 0.,
            a: 0.45,
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .max_w(px(560.))
                .rounded_md()
                .border_1()
                .border_color(*theme::BORDER)
                .bg(*theme::HEADER_BACKGROUND)
                .child(card),
        )
}

/// Decimal units, as in Dolphin: "1.2 GB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    // Lossy for sizes above 2^53 bytes, far beyond any disk.
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_decimal_units_without_rounding_up_to_1000() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1.0 kB");
        assert_eq!(format_bytes(1_234_567), "1.2 MB");
        assert_eq!(format_bytes(999_960), "1.0 MB");
        assert_eq!(format_bytes(52_400_000_000), "52.4 GB");
    }
}
