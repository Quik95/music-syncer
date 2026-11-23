use std::{
    mem,
    ops::Range,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FocusHandle, Focusable, MouseButton, Rgba,
    ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, actions, div, prelude::*,
    px, uniform_list,
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
    tree_diff::{self, Found, Node, Outline, Presence, Progress, Side, TreeDiff},
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
    comparison: Comparison,
    /// Set while the trees are still being read.
    comparing: Option<Comparing>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    copy: Option<Copy>,
    /// Outcome of the last copy, shown in the status bar.
    notice: Option<SharedString>,
    _compare: Task<()>,
    _refresh: Task<()>,
}

/// Trees being read in the background. Rows show up as entries are found; the toast with
/// the progress appears only once reading takes longer than [`PROGRESS_DELAY`], so quick
/// comparisons do not flash it.
struct Comparing {
    started: Instant,
    progress: Arc<Progress>,
    /// Found by the walk and not yet added to the comparison.
    found: Arc<Mutex<Vec<Found>>>,
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
    /// Reads both trees in the background, showing entries as they are found.
    pub fn new(left: Location, right: Location, cx: &mut Context<Self>) -> Self {
        let left_title = left.to_string().into();
        let right_title = right.to_string().into();
        let progress = Arc::new(Progress::default());
        let found = Arc::new(Mutex::new(Vec::new()));
        let compare = cx.spawn({
            let (left, right) = (left.clone(), right.clone());
            let (progress, found) = (progress.clone(), found.clone());
            async move |this, cx| {
                tracing::info!(%left, %right, "comparing trees");
                cx.background_executor()
                    .spawn(async move {
                        tree_diff::walk(&left, &right, &progress, &mut |item| {
                            if let Ok(mut found) = found.lock() {
                                found.push(item);
                            }
                        });
                    })
                    .await;
                _ = this.update(cx, |this, cx| this.finish(cx));
            }
        });
        // Redraws on every tick, not only when entries were found, as the toast's
        // animation pauses when the desktop asks for reduced motion.
        let refresh = cx.spawn(async move |this, cx| {
            while let Ok(true) = this.update(cx, |this, cx| this.take_found(cx)) {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
            }
        });
        let diff = TreeDiff::new(left, right);
        let outline = Outline::new(&diff, false);
        Self {
            left_title,
            right_title,
            comparison: Comparison {
                summary: summary(&diff, false),
                cursor: Cursor::new(outline.rows().len()),
                diff,
                outline,
            },
            comparing: Some(Comparing {
                started: Instant::now(),
                progress,
                found,
            }),
            _compare: compare,
            _refresh: refresh,
            copy: None,
            notice: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Adds the entries the walk found since the last call, keeping the active entry
    /// without scrolling. Returns whether the walk still runs.
    fn take_found(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(comparing) = &self.comparing else {
            return false;
        };
        let found = comparing
            .found
            .lock()
            .map(|mut found| mem::take(&mut *found))
            .unwrap_or_default();
        if !found.is_empty() {
            let active = self.active_node();
            let comparison = &mut self.comparison;
            let started_differing = comparison.diff.extend(found);
            comparison
                .outline
                .grow(&comparison.diff, &started_differing);
            comparison.summary = summary(&comparison.diff, false);
            self.reselect(active);
        }
        cx.notify();
        true
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.take_found(cx);
        self.comparing = None;
        self.comparison.summary = summary(&self.comparison.diff, true);
        tracing::info!(
            nodes = self.comparison.diff.nodes.len(),
            errors = self.comparison.diff.errors.len(),
            "trees compared"
        );
        cx.notify();
    }

    pub fn only_differences(&self) -> bool {
        self.comparison.outline.only_differences()
    }

    /// Shows only entries missing on a side and directories containing such entries.
    /// The active entry stays active, or the nearest visible one above it when hidden.
    pub fn set_only_differences(&mut self, only_differences: bool, cx: &mut Context<Self>) {
        let active = self.active_node();
        let comparison = &mut self.comparison;
        comparison
            .outline
            .set_only_differences(&comparison.diff, only_differences);
        self.keep_active(active);
        cx.notify();
    }

    /// Whether [`Self::request_copy`] would find anything to copy right now. Nothing can
    /// be copied before the comparison is complete.
    pub fn can_copy(&self, scope: CopyScope, target: Side) -> bool {
        if self.comparing.is_some() || self.copy.is_some() {
            return false;
        }
        let comparison = &self.comparison;
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
        let comparison = &self.comparison;
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
        let comparison = &mut self.comparison;
        comparison.diff.mark_on_both(&report.copied);
        comparison.outline.refresh(&comparison.diff);
        comparison.summary = summary(&comparison.diff, true);
        self.keep_active(active);
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

    /// [`Self::reselect`], scrolling `node` into view.
    fn keep_active(&mut self, node: Option<usize>) {
        if let Some(row) = self.reselect(node) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    /// Selects `node` again after the rows changed, or the nearest visible row above it;
    /// the first row when there was no active entry. Returns the row selected for `node`.
    fn reselect(&mut self, node: Option<usize>) -> Option<usize> {
        let comparison = &mut self.comparison;
        let len = comparison.outline.rows().len();
        comparison.cursor = Cursor::new(len);
        let row = comparison.outline.nearest_row(node?)?;
        comparison.cursor.select(row, len);
        Some(row)
    }

    /// Browsing is blocked while a copy is being confirmed or runs.
    fn browsable(&mut self) -> Option<&mut Comparison> {
        self.copy.is_none().then_some(&mut self.comparison)
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
        let comparison = &mut self.comparison;
        if let Some(row) = comparison.outline.row_of(node) {
            comparison
                .cursor
                .select(row, comparison.outline.rows().len());
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    fn active_node(&self) -> Option<usize> {
        let comparison = &self.comparison;
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
        if comparison.outline.is_expanded(node) && comparison.diff.has_children(node) {
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
        if !comparison.diff.has_children(node) {
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
        if comparison.diff.has_children(node) {
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
        let comparison = &self.comparison;
        if comparison.outline.rows().is_empty() {
            return match self.comparing {
                Some(_) => div(),
                None => div().px_2().child("(both empty)"),
            }
            .into_any_element();
        }
        let list = uniform_list(
            "rows",
            comparison.outline.rows().len(),
            cx.processor(|this, range: Range<usize>, window, cx| {
                let comparison = &this.comparison;
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
                        let disclosure = comparison.diff.has_children(index).then(|| {
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

/// How far reading the trees got, in the bottom right corner of the list area, which must
/// be `relative`. Covers only itself, so the rows found so far stay usable.
fn progress_toast(comparing: &Comparing) -> Div {
    let progress = &comparing.progress;
    div()
        .absolute()
        .bottom_2()
        .right(scrollbar::WIDTH + px(8.))
        .occlude()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(*theme::BORDER)
        .bg(*theme::HEADER_BACKGROUND)
        .shadow_lg()
        .child("Comparing trees…")
        .child(progress_bar(progress.fraction()))
        .child(
            div()
                .text_color(*theme::TEXT_MUTED)
                .child(format!("{} entries found", progress.entries())),
        )
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

/// Differences found in `diff`. Equality is claimed only once the walk is `complete` and
/// every directory could be read; before that the counts are marked provisional.
fn summary(diff: &TreeDiff, complete: bool) -> SharedString {
    let missing = diff.missing();
    let same = missing.left == 0 && missing.right == 0;
    let mut summary = match (complete, same) {
        (true, true) if diff.errors.is_empty() => "Same entries on both sides".to_owned(),
        (true, true) => "No differences found among readable entries".to_owned(),
        (true, false) => format!(
            "Missing on the left: {} · Missing on the right: {}",
            missing.left, missing.right
        ),
        (false, true) => "Comparing… no differences so far".to_owned(),
        (false, false) => format!(
            "Comparing… missing so far on the left: {} · on the right: {}",
            missing.left, missing.right
        ),
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
        let page = |this: &Self| page_rows(&this.scroll, this.comparison.outline.rows().len());
        let header = |title: SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .px_2()
                .py_1()
                .truncate()
                .child(title)
        };
        let summary = &self.comparison.summary;
        let status: SharedString = match &self.notice {
            Some(notice) => format!("{notice} · {summary}").into(),
            None => summary.clone(),
        };
        let toast = self
            .comparing
            .as_ref()
            .filter(|comparing| comparing.started.elapsed() >= PROGRESS_DELAY)
            .map(progress_toast);
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
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.body(cx))
                    .children(toast),
            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_diff::ReadError;

    #[test]
    fn unreadable_directory_prevents_claiming_equality() {
        let local = |path: &str| Location::local(path.into());
        let mut diff = TreeDiff::new(local("/left"), local("/right"));
        diff.extend([Found::Error(ReadError {
            path: local("/left/Album"),
            message: "Permission denied".to_owned(),
        })]);

        let status = summary(&diff, true);

        assert!(!status.contains("Same entries"), "{status}");
        assert!(status.contains("1 unreadable"), "{status}");
    }
}
