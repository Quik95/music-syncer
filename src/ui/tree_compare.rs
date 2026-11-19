use std::{ops::Range, path::PathBuf};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FocusHandle, Focusable, MouseButton, Rgba,
    ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, actions, div, prelude::*,
    px, uniform_list,
};

use super::{
    SelectFirst, SelectLast, SelectNext, SelectNextPage, SelectPrevious, SelectPreviousPage,
    page_rows, scrollbar, theme,
};
use crate::{
    cursor::{Cursor, Move},
    listing::EntryKind,
    tree_diff::{Node, Outline, Presence, TreeDiff},
};

pub const KEY_CONTEXT: &str = "TreeCompare";

actions!(tree_compare, [Collapse, Expand, Toggle]);

const INDENT: f32 = 16.;

/// Both trees merged into aligned rows, like a side-by-side diff: an entry missing on one
/// side leaves a gap there. Directories expand and collapse.
pub struct TreeCompare {
    left_title: SharedString,
    right_title: SharedString,
    state: State,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _compare: Task<()>,
}

enum State {
    Comparing,
    Ready(Comparison),
}

struct Comparison {
    diff: TreeDiff,
    outline: Outline,
    cursor: Cursor,
    summary: SharedString,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl TreeCompare {
    /// Reads both trees in the background.
    pub fn new(left: PathBuf, right: PathBuf, cx: &mut Context<Self>) -> Self {
        let left_title = left.display().to_string().into();
        let right_title = right.display().to_string().into();
        let task = cx.spawn(async move |this, cx| {
            tracing::info!(left = %left.display(), right = %right.display(), "comparing trees");
            let diff = cx
                .background_executor()
                .spawn(async move { TreeDiff::compare(&left, &right) })
                .await;
            tracing::info!(
                nodes = diff.nodes.len(),
                errors = diff.errors.len(),
                "trees compared"
            );
            _ = this.update(cx, |this, cx| {
                let outline = Outline::new(&diff);
                let cursor = Cursor::new(outline.rows().len());
                this.state = State::Ready(Comparison {
                    summary: summary(&diff),
                    diff,
                    outline,
                    cursor,
                });
                cx.notify();
            });
        });
        Self {
            left_title,
            right_title,
            state: State::Comparing,
            _compare: task,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn comparison(&mut self) -> Option<&mut Comparison> {
        match &mut self.state {
            State::Ready(comparison) => Some(comparison),
            State::Comparing => None,
        }
    }

    fn move_cursor(&mut self, step: Move, cx: &mut Context<Self>) {
        let Some(comparison) = self.comparison() else {
            return;
        };
        comparison
            .cursor
            .apply(step, comparison.outline.rows().len());
        let index = comparison.cursor.index();
        if let Some(index) = index {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn select(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        if let Some(comparison) = self.comparison() {
            comparison
                .cursor
                .select(row, comparison.outline.rows().len());
            cx.notify();
        }
    }

    fn select_node(&mut self, node: usize) {
        let Some(comparison) = self.comparison() else {
            return;
        };
        if let Some(row) = comparison.outline.row_of(node) {
            comparison
                .cursor
                .select(row, comparison.outline.rows().len());
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    fn active_node(&mut self) -> Option<usize> {
        let comparison = self.comparison()?;
        let row = comparison.cursor.index()?;
        Some(comparison.outline.rows()[row])
    }

    /// Collapses the active directory, or goes to the parent of an entry with nothing to collapse.
    fn collapse(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.active_node() else {
            return;
        };
        let Some(comparison) = self.comparison() else {
            return;
        };
        if comparison.outline.is_expanded(node) && comparison.diff.nodes[node].has_children(node) {
            comparison
                .outline
                .set_expanded(&comparison.diff, node, false);
        } else if let Some(parent) = comparison.diff.nodes[node].parent {
            self.select_node(parent);
        }
        cx.notify();
    }

    /// Expands the active directory, or goes to its first entry when already expanded.
    fn expand(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.active_node() else {
            return;
        };
        let Some(comparison) = self.comparison() else {
            return;
        };
        if !comparison.diff.nodes[node].has_children(node) {
            return;
        }
        if comparison.outline.is_expanded(node) {
            self.select_node(node + 1);
        } else {
            comparison
                .outline
                .set_expanded(&comparison.diff, node, true);
        }
        cx.notify();
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.active_node() else {
            return;
        };
        let Some(comparison) = self.comparison() else {
            return;
        };
        if comparison.diff.nodes[node].has_children(node) {
            let expanded = comparison.outline.is_expanded(node);
            comparison
                .outline
                .set_expanded(&comparison.diff, node, !expanded);
            cx.notify();
        }
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let comparison = match &self.state {
            State::Comparing => {
                return div().px_2().child("Comparing…").into_any_element();
            }
            State::Ready(comparison) if comparison.outline.rows().is_empty() => {
                return div().px_2().child("(both empty)").into_any_element();
            }
            State::Ready(comparison) => comparison,
        };
        let list = uniform_list(
            "rows",
            comparison.outline.rows().len(),
            cx.processor(|this, range: Range<usize>, window, cx| {
                let State::Ready(comparison) = &this.state else {
                    return Vec::new();
                };
                let active = comparison.cursor.index();
                let active_bg = if this.focus_handle.is_focused(window) {
                    *theme::ROW_ACTIVE
                } else {
                    *theme::ROW_ACTIVE_UNFOCUSED
                };
                let first = range.start;
                comparison.outline.rows()[range]
                    .iter()
                    .enumerate()
                    .map(|(offset, &index)| {
                        let row = first + offset;
                        let node = &comparison.diff.nodes[index];
                        let disclosure = node.has_children(index).then(|| {
                            if comparison.outline.is_expanded(index) {
                                "▾"
                            } else {
                                "▸"
                            }
                        });
                        let active_bg = (active == Some(row)).then_some(active_bg);
                        div()
                            .id(row)
                            .flex()
                            .w_full()
                            .hover(|style| style.bg(*theme::ROW_HOVER))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.select(row, window, cx);
                                if event.click_count() == 2 {
                                    this.toggle(cx);
                                }
                            }))
                            .child(
                                cell(node, disclosure, Side::Left, active_bg)
                                    .border_r_1()
                                    .border_color(*theme::BORDER),
                            )
                            .child(cell(node, disclosure, Side::Right, active_bg))
                    })
                    .collect()
            }),
        )
        .flex_1()
        .min_w_0()
        .h_full()
        .track_scroll(&self.scroll);
        let scrollbar =
            scrollbar::vertical_scrollbar("scrollbar", self.scroll.0.borrow().base_handle.clone());
        div()
            .flex()
            .size_full()
            .child(list)
            .child(scrollbar)
            .into_any_element()
    }
}

/// One side of a row: the entry indented by depth, or an empty gap where it is missing.
fn cell(node: &Node, disclosure: Option<&'static str>, side: Side, active_bg: Option<Rgba>) -> Div {
    // Fixed half rather than `flex_1`: flex adds the depth padding to each half's width.
    let cell = div().flex_none().w_1_2().overflow_hidden();
    let present = match side {
        Side::Left => node.presence.on_left(),
        Side::Right => node.presence.on_right(),
    };
    if !present {
        return cell.bg(*theme::MISSING_HERE);
    }
    let cell = match (active_bg, node.presence) {
        (Some(bg), _) => cell.bg(bg),
        (None, Presence::Both) => cell,
        (None, Presence::LeftOnly | Presence::RightOnly) => cell.bg(*theme::ONLY_HERE),
    };
    let color = match node.kind {
        _ if node.presence == Presence::Both && node.differs => *theme::DIFFERS,
        EntryKind::Directory => *theme::DIRECTORY,
        EntryKind::File => *theme::TEXT,
    };
    let name = match node.kind {
        EntryKind::Directory => format!("{}/", node.name),
        EntryKind::File => node.name.clone(),
    };
    cell.flex()
        .pl(px(8. + INDENT * node.depth as f32))
        .pr_2()
        .text_color(color)
        .child(
            div()
                .flex_none()
                .w(px(INDENT))
                .text_color(*theme::TEXT_MUTED)
                .children(disclosure),
        )
        .child(div().min_w_0().truncate().child(name))
}

fn summary(diff: &TreeDiff) -> SharedString {
    let missing = diff.missing();
    let mut summary = if missing.left == 0 && missing.right == 0 {
        "Same entries on both sides".to_owned()
    } else {
        format!(
            "Missing on the left: {} · Missing on the right: {}",
            missing.left, missing.right
        )
    };
    if let Some(first) = diff.errors.first() {
        summary += &format!(
            " · {} unreadable, first: {}: {}",
            diff.errors.len(),
            first.path.display(),
            first.message
        );
    }
    summary.into()
}

impl Focusable for TreeCompare {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TreeCompare {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = |this: &Self| match &this.state {
            State::Ready(comparison) => page_rows(&this.scroll, comparison.outline.rows().len()),
            State::Comparing => 1,
        };
        let header = |title: SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .px_2()
                .py_1()
                .truncate()
                .child(title)
        };
        let status = match &self.state {
            State::Ready(comparison) => comparison.summary.clone(),
            State::Comparing => SharedString::new_static(""),
        };
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_cursor(Move::Up(1), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_cursor(Move::Down(1), cx);
            }))
            .on_action(cx.listener(move |this, _: &SelectPreviousPage, _, cx| {
                this.move_cursor(Move::Up(page(this)), cx);
            }))
            .on_action(cx.listener(move |this, _: &SelectNextPage, _, cx| {
                this.move_cursor(Move::Down(page(this)), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.move_cursor(Move::First, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.move_cursor(Move::Last, cx);
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| this.collapse(cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| this.expand(cx)))
            .on_action(cx.listener(|this, _: &Toggle, _, cx| this.toggle(cx)))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .bg(*theme::HEADER_BACKGROUND)
                    .border_b_1()
                    .border_color(*theme::FOCUS)
                    .child(
                        header(self.left_title.clone())
                            .border_r_1()
                            .border_color(*theme::BORDER),
                    )
                    .child(header(self.right_title.clone()))
                    .child(div().flex_none().w(scrollbar::WIDTH)),
            )
            .child(div().flex_1().min_h_0().child(self.body(cx)))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(*theme::BORDER)
                    .bg(*theme::HEADER_BACKGROUND)
                    .text_color(*theme::TEXT_MUTED)
                    .truncate()
                    .child(status),
            )
    }
}
