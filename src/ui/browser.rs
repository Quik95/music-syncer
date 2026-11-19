use gpui::{
    AppContext as _, ClickEvent, Context, Div, Entity, Focusable as _, SharedString, Stateful,
    Window, actions, div, prelude::*,
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
            let left = self.left.read(cx).path().clone();
            let right = self.right.read(cx).path().clone();
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
        let filter = self.compare.as_ref().map(|compare| {
            let on = compare.read(cx).only_differences();
            let compare = compare.clone();
            button("only-differences", on)
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    compare.update(cx, |compare, cx| compare.set_only_differences(!on, cx));
                    cx.notify();
                }))
                .child("Only differences")
        });
        div()
            .flex()
            .flex_none()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(*theme::BORDER)
            .child(
                button("compare", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_compare(window, cx);
                    }))
                    .child(label),
            )
            .children(filter)
    }
}

/// A toolbar button; a `pressed` one stays highlighted, for toggles that are on.
fn button(id: &'static str, pressed: bool) -> Stateful<Div> {
    let background = if pressed {
        *theme::BUTTON_PRESSED
    } else {
        *theme::BUTTON
    };
    div()
        .id(id)
        .cursor_pointer()
        .px_2()
        .rounded_sm()
        .bg(background)
        .hover(|style| style.bg(*theme::BUTTON_HOVER))
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
