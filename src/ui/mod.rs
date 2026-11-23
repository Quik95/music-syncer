mod browser;
mod dir_column;
mod folder_picker;
mod scrollbar;
mod theme;
mod track_preview;
mod tree_compare;
mod widgets;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, UniformListScrollHandle,
    WindowBounds, WindowOptions, actions, px, size,
};
use gpui_platform::application;

use crate::listing::Listing;
use browser::Browser;

actions!(
    list,
    [
        SelectPrevious,
        SelectNext,
        SelectPreviousPage,
        SelectNextPage,
        SelectFirst,
        SelectLast,
    ]
);

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
    let compare = Some(tree_compare::KEY_CONTEXT);
    let browser = Some(browser::KEY_CONTEXT);
    for list in [column, compare] {
        cx.bind_keys([
            KeyBinding::new("up", SelectPrevious, list),
            KeyBinding::new("down", SelectNext, list),
            KeyBinding::new("pageup", SelectPreviousPage, list),
            KeyBinding::new("pagedown", SelectNextPage, list),
            KeyBinding::new("home", SelectFirst, list),
            KeyBinding::new("end", SelectLast, list),
        ]);
    }
    cx.bind_keys([
        KeyBinding::new("enter", dir_column::OpenSelected, column),
        KeyBinding::new("backspace", dir_column::OpenParent, column),
        KeyBinding::new("left", tree_compare::Collapse, compare),
        KeyBinding::new("right", tree_compare::Expand, compare),
        KeyBinding::new("enter", tree_compare::Toggle, compare),
        KeyBinding::new("shift-right", tree_compare::CopyActiveToRight, compare),
        KeyBinding::new("shift-left", tree_compare::CopyActiveToLeft, compare),
        KeyBinding::new("escape", tree_compare::Cancel, compare),
        KeyBinding::new("left", browser::FocusLeftColumn, browser),
        KeyBinding::new("right", browser::FocusRightColumn, browser),
        KeyBinding::new("tab", browser::FocusOtherColumn, browser),
        KeyBinding::new("shift-tab", browser::FocusOtherColumn, browser),
    ]);
}

/// Rows of a uniform list fully visible in its viewport during the last layout, at least one.
fn page_rows(scroll: &UniformListScrollHandle, len: usize) -> usize {
    let state = scroll.0.borrow();
    let Some(size) = state.last_item_size.filter(|_| len > 0) else {
        return 1;
    };
    let row_height = size.contents.height / len as f32;
    let rows = (size.item.height / row_height).floor();
    // Saturating float-to-int cast; NaN (zero-height rows) becomes 0.
    (rows as usize).saturating_sub(1).max(1)
}
