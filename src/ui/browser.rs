use gpui_kit::{
    AnyElement, ClickEvent, Entity, Focusable as _, Subscription, Window, actions,
    component::{
        ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
        button::Button,
        h_flex,
        toolbar::{Toolbar, ToolbarGroup},
        v_flex,
    },
    div,
    prelude::*,
};

use super::{
    dir_column::DirColumn,
    tree_compare::{Scope, TreeCompare},
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
        let comparing = self.compare.is_some();
        let toggle = Button::new("compare")
            .icon(if comparing {
                IconName::Close
            } else {
                IconName::Search
            })
            .label(if comparing {
                "Close comparison"
            } else {
                "Compare trees"
            })
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.toggle_compare(window, cx);
            }));
        let toolbar = Toolbar::new("toolbar").small().flex_wrap().child(toggle);
        let toolbar = match &self.compare {
            Some(compare) => Self::compare_groups(&compare.view, cx)
                .into_iter()
                .fold(toolbar, Toolbar::child),
            None => toolbar,
        };
        div()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(toolbar)
    }

    /// The differences filter and the copy actions, each enabled when it has work. A click
    /// returns focus to the comparison, so the keyboard keeps moving through its rows.
    fn compare_groups(compare: &Entity<TreeCompare>, cx: &mut Context<Self>) -> [ToolbarGroup; 4] {
        let view = compare.read(cx);
        let on = view.only_differences();
        let filter = Button::new("only-differences")
            .label("Only differences")
            .selected(on)
            .toggled(on)
            .on_click(cx.listener({
                let compare = compare.clone();
                move |_, _: &ClickEvent, window, cx| {
                    compare.update(cx, |compare, cx| compare.set_only_differences(!on, cx));
                    window.focus(&compare.focus_handle(cx), cx);
                }
            }));
        let check = |(id, label, scope)| {
            let compare = compare.clone();
            Button::new(id)
                .label(label)
                .disabled(!view.can_check(scope))
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    compare.update(cx, |compare, cx| compare.check_contents(scope, window, cx));
                    window.focus(&compare.focus_handle(cx), cx);
                }))
        };
        // The arrow shows the direction, so the spoken name has to say it. Overwriting only
        // goes to the right.
        let copy = |(id, label, name, scope, action): (_, _, _, _, Action)| {
            let compare = compare.clone();
            let icon = match action.target() {
                Side::Left => IconName::ArrowLeft,
                Side::Right => IconName::ArrowRight,
            };
            Button::new(id)
                .icon(icon)
                .label(label)
                .accessibility_label(name)
                .disabled(!view.can_run(scope, action))
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    compare.update(cx, |compare, cx| {
                        compare.request(scope, action, window, cx);
                    });
                }))
        };
        let to_right = Action::Copy {
            target: Side::Right,
        };
        let to_left = Action::Copy { target: Side::Left };
        #[rustfmt::skip]
        let copies = [
            ("copy-active-right", "Copy", "Copy to the right", Scope::Active, to_right),
            ("copy-active-left", "Copy", "Copy to the left", Scope::Active, to_left),
            ("copy-all-right", "Copy all missing", "Copy all missing to the right", Scope::All, to_right),
            ("copy-all-left", "Copy all missing", "Copy all missing to the left", Scope::All, to_left),
        ];
        #[rustfmt::skip]
        let overwrites = [
            ("overwrite-active", "Overwrite", "Overwrite on the right", Scope::Active, Action::Overwrite),
            ("overwrite-all", "Overwrite all changed", "Overwrite all changed on the right", Scope::All, Action::Overwrite),
        ];
        // Groups leave the spacing between their buttons to the caller. Buttons go into
        // the groups one by one, each boxed there, rather than as arrays of buttons.
        let group = |id, label| ToolbarGroup::new(id).label(label).gap_1().flex_wrap();
        [
            group("filter", "Filter").child(filter),
            group("check", "Compare contents").children(
                [
                    ("check-active", "Check contents", Scope::Active),
                    ("check-all", "Check all contents", Scope::All),
                ]
                .into_iter()
                .map(check),
            ),
            group("copy", "Copy").children(copies.into_iter().map(copy)),
            group("overwrite", "Overwrite").children(overwrites.into_iter().map(copy)),
        ]
    }

    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.compare {
            Some(compare) => div()
                .flex_1()
                .min_h_0()
                .child(compare.view.clone())
                .into_any_element(),
            None => h_flex()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .border_r_1()
                        .border_color(cx.theme().border)
                        .child(self.left.clone()),
                )
                .child(div().flex_1().min_w_0().child(self.right.clone()))
                .into_any_element(),
        }
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
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
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.toolbar(cx))
            .child(self.content(cx))
    }
}
