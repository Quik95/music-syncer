mod browser;
mod dir_column;
mod theme;

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::listing::Listing;
use browser::Browser;

pub fn run(left: Listing, right: Listing) {
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Browser::new(left, right, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
