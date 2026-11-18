mod browser;
mod dir_column;
mod scrollbar;
mod theme;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px,
    size,
};
use gpui_platform::application;

use crate::listing::Listing;
use browser::Browser;

pub fn run(left: Listing, right: Listing) {
    application().run(move |cx: &mut App| {
        bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(1000.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Music Syncer".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Browser::new(left, right, window, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}

fn bind_keys(cx: &mut App) {
    let column = Some(dir_column::KEY_CONTEXT);
    let browser = Some(browser::KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", dir_column::SelectPrevious, column),
        KeyBinding::new("down", dir_column::SelectNext, column),
        KeyBinding::new("pageup", dir_column::SelectPreviousPage, column),
        KeyBinding::new("pagedown", dir_column::SelectNextPage, column),
        KeyBinding::new("home", dir_column::SelectFirst, column),
        KeyBinding::new("end", dir_column::SelectLast, column),
        KeyBinding::new("left", browser::FocusLeftColumn, browser),
        KeyBinding::new("right", browser::FocusRightColumn, browser),
        KeyBinding::new("tab", browser::FocusOtherColumn, browser),
        KeyBinding::new("shift-tab", browser::FocusOtherColumn, browser),
    ]);
}
