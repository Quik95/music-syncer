//! Keyboard shortcuts of every view, each bound in the view's key context.

use gpui_kit::{App, KeyBinding};

use super::{
    browser, dir_column,
    list::{
        SelectFirst, SelectLast, SelectNext, SelectNextPage, SelectPrevious, SelectPreviousPage,
    },
    tree_compare,
};

pub fn bind(cx: &mut App) {
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
