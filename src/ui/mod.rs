mod assets;
mod browser;
mod dir_column;
mod keys;
mod list;
mod theme;
mod tree_compare;

use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, application, px,
    size,
};

use crate::storage::listing::Listing;
use assets::AppAssets;
use browser::Browser;

pub fn run(left: Listing, right: Listing) {
    application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::apply(cx);
            keys::bind(cx);
            let bounds = Bounds::centered(None, size(px(1000.), px(600.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Music Syncer".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Browser::new(left, right, window, cx)),
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}
