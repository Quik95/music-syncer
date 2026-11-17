use gpui::{
    App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use gpui_platform::application;

mod logging;

struct HelloWorld {
    text: SharedString,
}

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x2e3440))
            .text_xl()
            .text_color(rgb(0xeceff4))
            .child(format!("Hello, {}!", self.text))
    }
}

fn main() {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");

    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(300.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| HelloWorld {
                    text: "music-syncer".into(),
                })
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
