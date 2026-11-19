use gpui::{
    AppContext as _, ClickEvent, Context, Entity, Focusable as _, SharedString, Window, actions,
    div, prelude::*,
};

use super::{dir_column::DirColumn, theme, tree_compare::TreeCompare};
use crate::listing::Listing;

pub const KEY_CONTEXT: &str = "Browser";

actions!(
    browser,
    [FocusLeftColumn, FocusRightColumn, FocusOtherColumn]
);

/// Two directory listings side by side, or the comparison of their trees in their place.
/// The left listing starts focused.
pub struct Browser {
    left: Entity<DirColumn>,
    right: Entity<DirColumn>,
    compare: Option<Entity<TreeCompare>>,
}

impl Browser {
    pub fn new(left: Listing, right: Listing, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let left = cx.new(|cx| DirColumn::new(left, cx));
        let right = cx.new(|cx| DirColumn::new(right, cx));
        window.focus(&left.focus_handle(cx), cx);
        Self {
            left,
            right,
            compare: None,
        }
    }

    /// Ignored while comparing, when the columns are hidden.
    fn focus(&self, column: &Entity<DirColumn>, window: &mut Window, cx: &mut Context<Self>) {
        if self.compare.is_none() {
            window.focus(&column.focus_handle(cx), cx);
        }
    }

    /// Compares the trees under the directories the columns show, or goes back to them.
    fn toggle_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compare.take().is_some() {
            window.focus(&self.left.focus_handle(cx), cx);
        } else {
            let left = self.left.read(cx).path().to_path_buf();
            let right = self.right.read(cx).path().to_path_buf();
            let compare = cx.new(|cx| TreeCompare::new(left, right, cx));
            window.focus(&compare.focus_handle(cx), cx);
            self.compare = Some(compare);
        }
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = if self.compare.is_some() {
            "Close comparison".into()
        } else {
            "Compare trees".into()
        };
        div()
            .flex()
            .flex_none()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(*theme::BORDER)
            .child(
                div()
                    .id("compare")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_compare(window, cx);
                    }))
                    .cursor_pointer()
                    .px_2()
                    .rounded_sm()
                    .bg(*theme::BUTTON)
                    .hover(|style| style.bg(*theme::BUTTON_HOVER))
                    .child(label),
            )
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.compare {
            Some(compare) => div().flex_1().min_h_0().child(compare.clone()),
            None => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .border_r_1()
                        .border_color(*theme::BORDER)
                        .child(self.left.clone()),
                )
                .child(div().flex_1().min_w_0().child(self.right.clone())),
        };
        div()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &FocusLeftColumn, window, cx| {
                this.focus(&this.left.clone(), window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusRightColumn, window, cx| {
                this.focus(&this.right.clone(), window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusOtherColumn, window, cx| {
                let other = if this.left.focus_handle(cx).is_focused(window) {
                    this.right.clone()
                } else {
                    this.left.clone()
                };
                this.focus(&other, window, cx);
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(*theme::BACKGROUND)
            .text_color(*theme::TEXT)
            .child(self.toolbar(cx))
            .child(content)
    }
}
