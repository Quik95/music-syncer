use std::{ops::Range, sync::Arc, time::Duration};

use gpui::{
    AnyElement, App, AsyncApp, ClickEvent, Context, Div, FocusHandle, Focusable, MouseButton, Rgba,
    ScrollStrategy, SharedString, Task, UniformListScrollHandle, WeakEntity, Window, actions, div,
    prelude::*, px, uniform_list,
};

use super::{
    SelectFirst, SelectLast, SelectNext, SelectNextPage, SelectPrevious, SelectPreviousPage,
    page_rows, scrollbar, theme,
    widgets::{button, format_bytes, modal, progress_bar},
};
use crate::{
    cursor::{Cursor, Move},
    listing::EntryKind,
    location::Location,
    sync::{self, CopyProgress, Plan, Report},
    tree_diff::{Node, Outline, Presence, Progress, Side, TreeDiff},
};

pub const KEY_CONTEXT: &str = "TreeCompare";

actions!(
    tree_compare,
    [
        Collapse,
        Expand,
        Toggle,
        CopyActiveToLeft,
        CopyActiveToRight,
        CancelCopy,
    ]
);

const INDENT: f32 = 16.;
const PROGRESS_DELAY: Duration = Duration::from_millis(500);
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

/// Both trees merged into aligned rows, like a side-by-side diff: an entry missing on one
/// side leaves a gap there. Directories expand and collapse, and missing entries can be
/// copied over.
pub struct TreeCompare {
    left_title: SharedString,
    right_title: SharedString,
    state: State,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    only_differences: bool,
    copy: Option<Copy>,
    /// Outcome of the last copy, shown in the status bar.
    notice: Option<SharedString>,
    _compare: Task<()>,
    _show_progress: Task<()>,
}

enum State {
    /// The progress bar is `visible` only once the comparison takes longer than
    /// [`PROGRESS_DELAY`], so quick ones do not flash it.
    Comparing {
        progress: Arc<Progress>,
        visible: bool,
    },
    Ready(Box<Comparison>),
}

struct Comparison {
    diff: TreeDiff,
    outline: Outline,
    cursor: Cursor,
    summary: SharedString,
}

/// Which entries a copy takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CopyScope {
    /// The active entry, or everything missing below it when it is a directory on both sides.
    Active,
    All,
}

enum Copy {
    /// Waiting for the user to confirm; `free` is the space left on the target.
    Confirm {
        plan: Plan,
        free: Result<u64, String>,
    },
    Running(CopyJob),
}

/// A copy running in the background. Dropping it, e.g. by closing the comparison, cancels
/// it after the entry being copied.
struct CopyJob {
    target: Side,
    ops: usize,
    bytes: u64,
    progress: Arc<CopyProgress>,
    _run: Task<()>,
    _refresh: Task<()>,
}

impl Drop for CopyJob {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

impl TreeCompare {
    /// Reads both trees in the background.
    pub fn new(left: Location, right: Location, cx: &mut Context<Self>) -> Self {
        let left_title = left.to_string().into();
        let right_title = right.to_string().into();
        let progress = Arc::new(Progress::default());
        let compare = cx.spawn({
            let progress = progress.clone();
            async move |this, cx| {
                tracing::info!(%left, %right, "comparing trees");
                let diff = cx
                    .background_executor()
                    .spawn(async move { TreeDiff::compare(&left, &right, &progress) })
                    .await;
                Self::finish(this, diff, cx);
            }
        });
        // Also redraws while the bar is shown, since its animation pauses when the
        // desktop asks for reduced motion.
        let show_progress = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROGRESS_DELAY).await;
            while let Ok(true) = this.update(cx, |this, cx| {
                let State::Comparing { visible, .. } = &mut this.state else {
                    return false;
                };
                *visible = true;
                cx.notify();
                true
            }) {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
            }
        });
        Self {
            left_title,
            right_title,
            state: State::Comparing {
                progress,
                visible: false,
            },
            _compare: compare,
            _show_progress: show_progress,
            only_differences: false,
            copy: None,
            notice: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn finish(this: WeakEntity<Self>, diff: TreeDiff, cx: &mut AsyncApp) {
        tracing::info!(
            nodes = diff.nodes.len(),
            errors = diff.errors.len(),
            "trees compared"
        );
        _ = this.update(cx, |this, cx| {
            let outline = Outline::new(&diff, this.only_differences);
            let cursor = Cursor::new(outline.rows().len());
            this.state = State::Ready(Box::new(Comparison {
                summary: summary(&diff),
                diff,
                outline,
                cursor,
            }));
            cx.notify();
        });
    }

    pub fn only_differences(&self) -> bool {
        self.only_differences
    }

    /// Shows only entries missing on a side and directories containing such entries.
    /// The active entry stays active, or the nearest visible one above it when hidden.
    pub fn set_only_differences(&mut self, only_differences: bool, cx: &mut Context<Self>) {
        self.only_differences = only_differences;
        let active = self.active_node();
        let Some(comparison) = self.comparison() else {
            return;
        };
        comparison
            .outline
            .set_only_differences(&comparison.diff, only_differences);
        self.keep_active(active);
        cx.notify();
    }

    /// Whether [`Self::request_copy`] would find anything to copy right now.
    pub fn can_copy(&self, scope: CopyScope, target: Side) -> bool {
        let State::Ready(comparison) = &self.state else {
            return false;
        };
        if self.copy.is_some() {
            return false;
        }
        match scope {
            CopyScope::All => sync::has_work(&comparison.diff, None, target),
            CopyScope::Active => comparison.cursor.index().is_some_and(|row| {
                let node = comparison.outline.rows()[row];
                sync::has_work(&comparison.diff, Some(node), target)
            }),
        }
    }

    /// Asks for confirmation to copy the entries in `scope` that `target` lacks.
    pub fn request_copy(
        &mut self,
        scope: CopyScope,
        target: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_copy(scope, target) {
            return;
        }
        let active = self.active_node();
        let Some(comparison) = self.comparison() else {
            return;
        };
        let scope = match scope {
            CopyScope::All => None,
            CopyScope::Active => active,
        };
        let plan = Plan::new(&comparison.diff, scope, target);
        let free =
            sync::free_space(comparison.diff.root(target)).map_err(|error| format!("{error:#}"));
        self.copy = Some(Copy::Confirm { plan, free });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn confirm_copy(&mut self, cx: &mut Context<Self>) {
        let Some(Copy::Confirm { plan, .. }) = self.copy.take() else {
            return;
        };
        tracing::info!(
            target = side_name(plan.target),
            files = plan.files,
            dirs = plan.dirs,
            bytes = plan.bytes,
            "copying"
        );
        let progress = Arc::new(CopyProgress::default());
        let (target, ops, bytes) = (plan.target, plan.ops.len(), plan.bytes);
        let run = cx.spawn({
            let progress = progress.clone();
            async move |this, cx| {
                let report = cx
                    .background_executor()
                    .spawn(async move { sync::execute(&plan, &progress) })
                    .await;
                _ = this.update(cx, |this, cx| this.finish_copy(&report, cx));
            }
        });
        let refresh = cx.spawn(async move |this, cx| {
            while let Ok(true) = this.update(cx, |this, cx| {
                cx.notify();
                matches!(this.copy, Some(Copy::Running(_)))
            }) {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
            }
        });
        self.copy = Some(Copy::Running(CopyJob {
            target,
            ops,
            bytes,
            progress,
            _run: run,
            _refresh: refresh,
        }));
        cx.notify();
    }

    fn finish_copy(&mut self, report: &Report, cx: &mut Context<Self>) {
        self.copy = None;
        tracing::info!(
            copied = report.copied.len(),
            failed = report.failed.len(),
            skipped = report.skipped,
            cancelled = report.cancelled,
            "copy finished"
        );
        let active = self.active_node();
        if let Some(comparison) = self.comparison() {
            comparison.diff.mark_on_both(&report.copied);
            comparison.outline.refresh(&comparison.diff);
            comparison.summary = summary(&comparison.diff);
            self.keep_active(active);
        }
        self.notice = Some(copy_notice(report).into());
        cx.notify();
    }

    fn cancel_copy(&mut self, cx: &mut Context<Self>) {
        match &self.copy {
            Some(Copy::Confirm { .. }) => self.copy = None,
            Some(Copy::Running(job)) => job.progress.cancel(),
            None => return,
        }
        cx.notify();
    }

    /// Selects `node` again after the rows changed, or the nearest visible row above it.
    fn keep_active(&mut self, node: Option<usize>) {
        let Some(comparison) = self.comparison() else {
            return;
        };
        let len = comparison.outline.rows().len();
        comparison.cursor = Cursor::new(len);
        if let Some(row) = node.and_then(|node| comparison.outline.nearest_row(node)) {
            comparison.cursor.select(row, len);
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    fn comparison(&mut self) -> Option<&mut Comparison> {
        match &mut self.state {
            State::Ready(comparison) => Some(comparison),
            State::Comparing { .. } => None,
        }
    }

    /// Browsing is blocked while a copy is being confirmed or runs.
    fn browsable(&mut self) -> Option<&mut Comparison> {
        if self.copy.is_some() {
            return None;
        }
        self.comparison()
    }

    fn move_cursor(&mut self, step: Move, cx: &mut Context<Self>) {
        let Some(comparison) = self.browsable() else {
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
        if let Some(comparison) = self.browsable() {
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
        let Some(comparison) = self.browsable() else {
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
        let Some(comparison) = self.browsable() else {
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

    /// Expands or collapses the active directory; confirms a pending copy instead.
    fn toggle(&mut self, cx: &mut Context<Self>) {
        if matches!(self.copy, Some(Copy::Confirm { .. })) {
            if self.copy_fits() {
                self.confirm_copy(cx);
            }
            return;
        }
        let Some(node) = self.active_node() else {
            return;
        };
        let Some(comparison) = self.browsable() else {
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

    /// The pending copy fits into the target's free space, when that is known.
    fn copy_fits(&self) -> bool {
        match &self.copy {
            Some(Copy::Confirm { plan, free }) => {
                free.as_ref().map_or(true, |&free| plan.bytes <= free)
            }
            _ => false,
        }
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let comparison = match &self.state {
            State::Comparing { progress, visible } => {
                return if *visible {
                    centered_progress(
                        progress.fraction(),
                        format!("Comparing… {} entries", progress.entries()),
                    )
                    .into_any_element()
                } else {
                    div().into_any_element()
                };
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

    fn copy_dialog(&self, cx: &mut Context<Self>) -> Option<Div> {
        let card =
            match self.copy.as_ref()? {
                Copy::Confirm { plan, free } => {
                    let fits = self.copy_fits();
                    let (from, to) = self.titles(plan.target);
                    let free = match free {
                        Ok(free) if fits => {
                            div().child(format!("Free on the target: {}", format_bytes(*free)))
                        }
                        Ok(free) => div().text_color(*theme::ERROR).child(format!(
                            "Not enough space: {} free on the target",
                            format_bytes(*free)
                        )),
                        Err(error) => div()
                            .text_color(*theme::TEXT_MUTED)
                            .child(format!("Free space unknown: {error}")),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(format!(
                            "Copy {} to the {}?",
                            describe(plan.files, plan.dirs, plan.bytes),
                            side_name(plan.target)
                        ))
                        .child(
                            div()
                                .text_color(*theme::TEXT_MUTED)
                                .child(format!("From {from}"))
                                .child(format!("To {to}")),
                        )
                        .child(free)
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(button("copy-cancel", false, true).child("Cancel").on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_copy(cx)),
                                ))
                                .child(button("copy-confirm", true, fits).child("Copy").on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        if this.copy_fits() {
                                            this.confirm_copy(cx);
                                        }
                                    }),
                                )),
                        )
                }
                Copy::Running(job) => {
                    let done = job.progress.ops();
                    let fraction = if job.bytes > 0 {
                        job.progress.bytes() as f32 / job.bytes as f32
                    } else {
                        done as f32 / job.ops.max(1) as f32
                    };
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(format!("Copying to the {}…", side_name(job.target)))
                        .child(progress_bar(fraction))
                        .child(
                            div()
                                .text_color(*theme::TEXT_MUTED)
                                .max_w(px(480.))
                                .truncate()
                                .child(format!(
                                    "{} of {} · {}",
                                    (done + 1).min(job.ops),
                                    job.ops,
                                    job.progress.current()
                                )),
                        )
                        .child(button("copy-stop", false, true).child("Stop").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_copy(cx)),
                        ))
                }
            };
        Some(modal(card))
    }

    /// Column titles of the side opposite to `target` and of `target`.
    fn titles(&self, target: Side) -> (&SharedString, &SharedString) {
        match target {
            Side::Left => (&self.right_title, &self.left_title),
            Side::Right => (&self.left_title, &self.right_title),
        }
    }
}

/// One side of a row: the entry indented by depth, or an empty gap where it is missing.
fn cell(node: &Node, disclosure: Option<&'static str>, side: Side, active_bg: Option<Rgba>) -> Div {
    // Fixed half rather than `flex_1`: flex adds the depth padding to each half's width.
    let cell = div().flex_none().w_1_2().overflow_hidden();
    if !node.presence.is_on(side) {
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
        EntryKind::File { .. } => *theme::TEXT,
    };
    let name = match node.kind {
        EntryKind::Directory => format!("{}/", node.name),
        EntryKind::File { .. } => node.name.clone(),
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

/// A progress bar with a caption, centered in the list area.
fn centered_progress(fraction: f32, caption: String) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(progress_bar(fraction))
        .child(div().text_color(*theme::TEXT_MUTED).child(caption))
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

/// "3 files and 1 folder (12.3 MB)".
fn describe(files: usize, dirs: usize, bytes: u64) -> String {
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(count(files, "file", "files"));
    }
    if dirs > 0 {
        parts.push(count(dirs, "folder", "folders"));
    }
    format!("{} ({})", parts.join(" and "), format_bytes(bytes))
}

fn copy_notice(report: &Report) -> String {
    let mut notice = format!("Copied {} entries", report.copied.len());
    if report.cancelled {
        notice += ", stopped";
    }
    if report.skipped > 0 {
        notice += &format!(", {} skipped", report.skipped);
    }
    if let Some(first) = report.failed.first() {
        notice += &format!(
            ", {} failed, first: {}: {}",
            report.failed.len(),
            first.target,
            first.message
        );
    }
    notice
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
            first.path,
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
            State::Comparing { .. } => 1,
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
        let status: SharedString = match (&self.state, &self.notice) {
            (State::Ready(comparison), Some(notice)) => {
                format!("{notice} · {}", comparison.summary).into()
            }
            (State::Ready(comparison), None) => comparison.summary.clone(),
            (State::Comparing { .. }, _) => SharedString::new_static(""),
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
            .on_action(cx.listener(|this, _: &CopyActiveToLeft, window, cx| {
                this.request_copy(CopyScope::Active, Side::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyActiveToRight, window, cx| {
                this.request_copy(CopyScope::Active, Side::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CancelCopy, _, cx| this.cancel_copy(cx)))
            .relative()
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
            .children(self.copy_dialog(cx))
    }
}
