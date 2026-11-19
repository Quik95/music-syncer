use gpui::{AppContext as _, Context, Entity, Focusable as _, Window, actions, div, prelude::*};

use super::{dir_column::DirColumn, theme};
use crate::listing::Listing;

pub const KEY_CONTEXT: &str = "Browser";

actions!(
    browser,
    [FocusLeftColumn, FocusRightColumn, FocusOtherColumn]
);

/// Two directory listings side by side; the left one starts focused.
pub struct Browser {
    left: Entity<DirColumn>,
    right: Entity<DirColumn>,
}

impl Browser {
    pub fn new(left: Listing, right: Listing, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let left = cx.new(|cx| DirColumn::new(left, cx));
        let right = cx.new(|cx| DirColumn::new(right, cx));
        window.focus(&left.focus_handle(cx), cx);
        Self { left, right }
    }

    fn focus(column: &Entity<DirColumn>, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&column.focus_handle(cx), cx);
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &FocusLeftColumn, window, cx| {
                Self::focus(&this.left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusRightColumn, window, cx| {
                Self::focus(&this.right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusOtherColumn, window, cx| {
                let other = if this.left.focus_handle(cx).is_focused(window) {
                    &this.right
                } else {
                    &this.left
                };
                Self::focus(other, window, cx);
            }))
            .flex()
            .size_full()
            .bg(*theme::BACKGROUND)
            .text_color(*theme::TEXT)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .border_r_1()
                    .border_color(*theme::BORDER)
                    .child(self.left.clone()),
            )
            .child(div().flex_1().min_w_0().child(self.right.clone()))
    }
}
