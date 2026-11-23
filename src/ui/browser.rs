use gpui::{
    AppContext as _, ClickEvent, Context, Div, Entity, Focusable as _, SharedString, Stateful,
    Subscription, Window, actions, div, prelude::*,
};

use super::{
    dir_column::DirColumn,
    theme,
    tree_compare::{Scope, TreeCompare},
    widgets::button,
};
use crate::{listing::Listing, sync::Action, tree_diff::Side};

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
    compare: Option<OpenComparison>,
}

struct OpenComparison {
    view: Entity<TreeCompare>,
    /// Re-renders the toolbar, which shows the comparison's available actions.
    _changes: Subscription,
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
            let view = cx.new(|cx| TreeCompare::new(left, right, cx));
            window.focus(&view.focus_handle(cx), cx);
            self.compare = Some(OpenComparison {
                _changes: cx.observe(&view, |_, _, cx| cx.notify()),
                view,
            });
        }
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = if self.compare.is_some() {
            "Close comparison".into()
        } else {
            "Compare trees".into()
        };
        let compare_buttons = self
            .compare
            .as_ref()
            .map(|compare| Self::compare_buttons(&compare.view, cx))
            .unwrap_or_default();
        div()
            .flex()
            .flex_wrap()
            .flex_none()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(*theme::BORDER)
            .child(
                button("compare", false, true)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_compare(window, cx);
                    }))
                    .child(label),
            )
            .children(compare_buttons)
    }

    /// The differences filter and the copy actions, each enabled when it has work.
    fn compare_buttons(
        compare: &Entity<TreeCompare>,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let view = compare.read(cx);
        let on = view.only_differences();
        let filter = button("only-differences", on, true)
            .on_click(cx.listener({
                let compare = compare.clone();
                move |_, _: &ClickEvent, _, cx| {
                    compare.update(cx, |compare, cx| compare.set_only_differences(!on, cx));
                }
            }))
            .child("Only differences");
        let checks = [
            ("check-active", "Check contents", Scope::Active),
            ("check-all", "Check all contents", Scope::All),
        ]
        .map(|(id, label, scope)| {
            let enabled = view.can_check(scope);
            let compare = compare.clone();
            button(id, false, enabled)
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    compare.update(cx, |compare, cx| compare.check_contents(scope, cx));
                }))
                .child(label)
        });
        let to_right = Action::Copy {
            target: Side::Right,
        };
        let to_left = Action::Copy { target: Side::Left };
        let actions = [
            ("copy-active-right", "Copy →", Scope::Active, to_right),
            ("copy-active-left", "← Copy", Scope::Active, to_left),
            ("copy-all-right", "Copy all missing →", Scope::All, to_right),
            ("copy-all-left", "← Copy all missing", Scope::All, to_left),
            (
                "overwrite-active",
                "Overwrite →",
                Scope::Active,
                Action::Overwrite,
            ),
            (
                "overwrite-all",
                "Overwrite all changed →",
                Scope::All,
                Action::Overwrite,
            ),
        ]
        .map(|(id, label, scope, action)| {
            let enabled = view.can_run(scope, action);
            let compare = compare.clone();
            button(id, false, enabled)
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    compare.update(cx, |compare, cx| {
                        compare.request(scope, action, window, cx);
                    });
                }))
                .child(label)
        });
        std::iter::once(filter)
            .chain(checks)
            .chain(actions)
            .collect()
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.compare {
            Some(compare) => div().flex_1().min_h_0().child(compare.view.clone()),
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
