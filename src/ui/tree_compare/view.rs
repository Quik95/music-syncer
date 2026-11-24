//! Drawing the comparison: aligned rows, progress toasts and outcome notifications.

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, Hsla, MouseButton, SharedString, Window,
    component::{
        ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
        button::Button,
        empty::{Empty, EmptyHeader, EmptyTitle},
        h_flex,
        list::ListItem,
        notification::Notification,
        progress::Progress as ProgressBar,
        scroll::ScrollableElement as _,
        status_bar::StatusBar,
        v_flex,
    },
    div,
    prelude::*,
    px, uniform_list,
};

use super::{
    Cancel, Collapse, CopyActiveToLeft, CopyActiveToRight, Expand, KEY_CONTEXT, PROGRESS_DELAY,
    Scope, Toggle, TreeCompare,
};
use crate::{
    diff::{Content, Node, Presence, Side},
    progress::JobProgress,
    storage::listing::EntryKind,
    sync::Action,
    ui::{
        list::{
            Move, SelectFirst, SelectLast, SelectNext, SelectNextPage, SelectPrevious,
            SelectPreviousPage, page_rows,
        },
        theme,
    },
};

const INDENT: f32 = 16.;

impl TreeCompare {
    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let comparison = &self.comparison;
        if comparison.outline.rows().is_empty() {
            return match self.comparing {
                Some(_) => div().into_any_element(),
                None => Empty::new()
                    .size_full()
                    .header(EmptyHeader::new().title(EmptyTitle::new().child("Both empty")))
                    .into_any_element(),
            };
        }
        let list = uniform_list(
            "rows",
            comparison.outline.rows().len(),
            cx.processor(|this, range: Range<usize>, window, cx| {
                let comparison = &this.comparison;
                let active = comparison.cursor.index();
                let focused = this.focus_handle.is_focused(window);
                let first = range.start;
                comparison.outline.rows()[range]
                    .iter()
                    .enumerate()
                    .map(|(offset, &index)| {
                        let row = first + offset;
                        let node = &comparison.diff.nodes[index];
                        let disclosure = comparison.diff.has_children(index).then(|| {
                            if comparison.outline.is_expanded(index) {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }
                        });
                        let is_active = active == Some(row);
                        let item = ListItem::new(row)
                            .w_full()
                            .p_0()
                            .gap_0()
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.select(row, window, cx);
                                if event.click_count() == 2 {
                                    this.toggle(cx);
                                }
                            }));
                        // Unfocused, the active row paints its own background on the
                        // cells, which cover the item: the item's hover would replace a
                        // background set on the item itself.
                        let cells = h_flex()
                            .w_full()
                            .items_stretch()
                            .when(is_active && !focused, |cells| {
                                cells.bg(*theme::ROW_ACTIVE_UNFOCUSED)
                            })
                            .child(
                                cell(node, disclosure.clone(), Side::Left, is_active, cx)
                                    .border_r_1()
                                    .border_color(cx.theme().border),
                            )
                            .child(cell(node, disclosure, Side::Right, is_active, cx));
                        item.selected(is_active && focused).child(cells)
                    })
                    .collect()
            }),
        )
        .size_full()
        .track_scroll(&self.scroll);
        div()
            .relative()
            .size_full()
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    /// Progress of reading the trees, once it takes longer than [`PROGRESS_DELAY`], or of
    /// comparing contents, with a button to stop it.
    fn toast(&self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(comparing) = &self.comparing {
            let progress = &comparing.progress;
            return (comparing.started.elapsed() >= PROGRESS_DELAY).then(|| {
                toast(
                    "Comparing trees…",
                    progress.fraction(),
                    format!("{} entries found", progress.entries()),
                    cx,
                )
            });
        }
        let job = self.checking.as_ref()?;
        let done = job.progress.done();
        let stop = Button::new("check-stop")
            .small()
            .label("Stop")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)));
        Some(
            toast(
                "Comparing contents…",
                fraction(&job.progress, done, job.files, job.bytes),
                format!(
                    "{} of {} · {}",
                    (done + 1).min(job.files),
                    job.files,
                    job.progress.current()
                ),
                cx,
            )
            .child(h_flex().justify_end().child(stop)),
        )
    }
}

/// One side of a row: the entry indented by depth, or an empty gap where it is missing.
/// Sides of the active row leave the row's selection showing, gaps excepted.
fn cell(node: &Node, disclosure: Option<IconName>, side: Side, active: bool, cx: &App) -> Div {
    // Fixed half rather than `flex_1`: flex adds the depth padding to each half's width.
    let cell = div().flex_none().w_1_2().overflow_hidden();
    if !node.presence.is_on(side) {
        return cell.bg(*theme::MISSING_HERE);
    }
    let cell = match node.presence {
        _ if active => cell,
        Presence::Both if node.content == Some(Content::Changed) => cell.bg(*theme::CHANGED),
        Presence::Both => cell,
        Presence::LeftOnly | Presence::RightOnly => cell.bg(*theme::ONLY_HERE),
    };
    let muted = cx.theme().muted_foreground;
    let (icon, icon_color): (_, Hsla) = match node.kind {
        EntryKind::Directory => (IconName::Folder, (*theme::DIRECTORY).into()),
        EntryKind::File { .. } => (IconName::File, muted),
    };
    let color = match node.kind {
        _ if node.presence == Presence::Both && node.differs => (*theme::DIFFERS).into(),
        EntryKind::Directory => icon_color,
        EntryKind::File { .. } => cx.theme().foreground,
    };
    cell.flex()
        .items_center()
        .gap_1()
        .py_1()
        .pl(px(8. + INDENT * node.depth as f32))
        .pr_2()
        .text_color(color)
        .child(
            div()
                .flex_none()
                .w(px(INDENT))
                .children(disclosure.map(|icon| Icon::new(icon).xsmall().text_color(muted))),
        )
        .child(Icon::new(icon).small().text_color(icon_color))
        .child(div().min_w_0().truncate().child(node.name.clone()))
}

/// A progress card in the bottom right corner of the list area, which must be `relative`.
/// Covers only itself, so the rows stay usable.
fn toast(title: &'static str, fraction: f32, caption: String, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .absolute()
        .bottom_2()
        .right_4()
        .w(px(340.))
        .occlude()
        .gap_2()
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .child(title)
        .child(ProgressBar::new("toast-progress").value(fraction * 100.))
        .child(
            div()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(caption),
        )
}

/// Reports how a copy or content check ended. A complete one fades away on its own; one
/// that stopped early or failed somewhere stays until dismissed.
pub(super) fn push_outcome(
    title: &'static str,
    message: String,
    complete: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let notification = if complete {
        Notification::success(message)
    } else {
        Notification::warning(message).autohide(false)
    };
    window.push_notification(notification.title(title), cx);
}

/// Share of a job done: by bytes when it has any, else by entries.
pub(super) fn fraction(progress: &JobProgress, done: usize, total: usize, bytes: u64) -> f32 {
    if bytes > 0 {
        progress.bytes() as f32 / bytes as f32
    } else {
        done as f32 / total.max(1) as f32
    }
}

impl Render for TreeCompare {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = |this: &Self| page_rows(&this.scroll, this.comparison.outline.rows().len());
        let header = |title: SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_1()
                .truncate()
                .child(title)
        };
        let theme = cx.theme();
        let header_border = if self.focus_handle.is_focused(window) {
            theme.ring
        } else {
            theme.border
        };
        let headers = h_flex()
            .flex_none()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(header_border)
            .child(
                header(self.left_title.clone())
                    .border_r_1()
                    .border_color(theme.border),
            )
            .child(header(self.right_title.clone()));
        let status = StatusBar::new().child(
            div()
                .min_w_0()
                .truncate()
                .child(self.comparison.summary.clone()),
        );
        let toast = self.toast(cx);
        v_flex()
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
            .on_action(cx.listener(|this, _: &CopyActiveToLeft, window, cx| {
                let action = Action::Copy { target: Side::Left };
                this.request(Scope::Active, action, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyActiveToRight, window, cx| {
                let action = Action::Copy {
                    target: Side::Right,
                };
                this.request(Scope::Active, action, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
            .size_full()
            .child(headers)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.body(cx))
                    .children(toast),
            )
            .child(status)
    }
}
