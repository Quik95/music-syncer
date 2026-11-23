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
    content::{self, CheckPlan, Verdict},
    cursor::{Cursor, Move},
    listing::EntryKind,
    location::Location,
    progress::JobProgress,
    sync::{self, Action, Plan, Report},
    tree_diff::{self, Content, Found, Missing, Node, Outline, Presence, Progress, Side, TreeDiff},
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
        Cancel,
    ]
);

const INDENT: f32 = 16.;
const PROGRESS_DELAY: Duration = Duration::from_millis(500);
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

/// Both trees merged into aligned rows, like a side-by-side diff: an entry missing on one
/// side leaves a gap there. Directories expand and collapse, missing entries can be
/// copied over, and files on both sides can be compared by contents and overwritten on
/// the right with the left's version.
pub struct TreeCompare {
    left_title: SharedString,
    right_title: SharedString,
    comparison: Comparison,
    /// Set while the trees are still being read.
    comparing: Option<Comparing>,
    /// Set while file contents are being compared.
    checking: Option<CheckJob>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    copy: Option<Copy>,
    /// Outcome of the last copy or content check, shown in the status bar.
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

/// Which entries a copy, overwrite or content check takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The active entry, or what applies below it when it is a directory on both sides.
    Active,
    All,
}

/// A copy or overwrite.
enum Copy {
    /// Waiting for the user to confirm; `free` is the space left on the target.
    Confirm {
        plan: Plan,
        free: Result<u64, String>,
    },
    Running(CopyJob),
}

/// A copy or overwrite running in the background. Dropping it, e.g. by closing the
/// comparison, cancels it after the entry being copied.
struct CopyJob {
    action: Action,
    ops: usize,
    bytes: u64,
    progress: Arc<JobProgress>,
    _run: Task<()>,
    _refresh: Task<()>,
}

impl Drop for CopyJob {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

/// File contents being compared in the background; verdicts show up as they come.
/// Dropping it cancels it after the file being compared.
struct CheckJob {
    files: usize,
    bytes: u64,
    progress: Arc<JobProgress>,
    /// Verdicts not yet recorded in the comparison.
    checked: Arc<Mutex<Vec<(usize, Verdict)>>>,
    /// Verdicts recorded so far: pairs compared, the changed ones among them, and pairs
    /// that could not be compared.
    compared: usize,
    changed: usize,
    failed: usize,
    /// Why the first pair that could not be compared failed, naming the file.
    first_error: Option<String>,
    _run: Task<()>,
    _refresh: Task<()>,
}

impl Drop for CheckJob {
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
            checking: None,
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

    /// The node `scope` starts at, `None` for the whole tree; `Err` when it is the active
    /// entry and there is none.
    fn scope_node(&self, scope: Scope) -> Result<Option<usize>, ()> {
        match scope {
            Scope::All => Ok(None),
            Scope::Active => self.active_node().map(Some).ok_or(()),
        }
    }

    /// Nothing starts while the trees are read or another job runs.
    fn idle(&self) -> bool {
        self.comparing.is_none() && self.checking.is_none() && self.copy.is_none()
    }

    /// Whether [`Self::request`] would find anything to do right now.
    pub fn can_run(&self, scope: Scope, action: Action) -> bool {
        self.idle()
            && self
                .scope_node(scope)
                .is_ok_and(|node| sync::has_work(&self.comparison.diff, action, node))
    }

    /// Asks for confirmation to run `action` on the entries in `scope`.
    pub fn request(
        &mut self,
        scope: Scope,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_run(scope, action) {
            return;
        }
        let Ok(node) = self.scope_node(scope) else {
            return;
        };
        let diff = &self.comparison.diff;
        let plan = Plan::new(diff, action, node);
        let free =
            sync::free_space(diff.root(action.target())).map_err(|error| format!("{error:#}"));
        self.copy = Some(Copy::Confirm { plan, free });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Whether [`Self::check_contents`] would find files to compare right now.
    pub fn can_check(&self, scope: Scope) -> bool {
        self.idle()
            && self
                .scope_node(scope)
                .is_ok_and(|node| content::has_work(&self.comparison.diff, node))
    }

    /// Compares the bytes of the files in `scope` that are on both sides with equal sizes,
    /// in the background.
    pub fn check_contents(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if !self.can_check(scope) {
            return;
        }
        let Ok(node) = self.scope_node(scope) else {
            return;
        };
        let plan = CheckPlan::new(&self.comparison.diff, node);
        tracing::info!(
            files = plan.checks.len(),
            bytes = plan.bytes,
            "comparing contents"
        );
        let progress = Arc::new(JobProgress::default());
        let checked = Arc::new(Mutex::new(Vec::new()));
        let (files, bytes) = (plan.checks.len(), plan.bytes);
        let run = cx.spawn({
            let (progress, checked) = (progress.clone(), checked.clone());
            async move |this, cx| {
                cx.background_executor()
                    .spawn(async move {
                        content::execute(&plan, &progress, &mut |node, verdict| {
                            if let Ok(mut checked) = checked.lock() {
                                checked.push((node, verdict));
                            }
                        });
                    })
                    .await;
                _ = this.update(cx, |this, cx| this.finish_check(cx));
            }
        });
        let refresh = cx.spawn(async move |this, cx| {
            while let Ok(true) = this.update(cx, |this, cx| this.take_checked(cx)) {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
            }
        });
        self.checking = Some(CheckJob {
            files,
            bytes,
            progress,
            checked,
            compared: 0,
            changed: 0,
            failed: 0,
            first_error: None,
            _run: run,
            _refresh: refresh,
        });
        self.notice = None;
        cx.notify();
    }

    /// Records the verdicts reached since the last call, keeping the active entry without
    /// scrolling. Returns whether the check still runs.
    fn take_checked(&mut self, cx: &mut Context<Self>) -> bool {
        let active = self.active_node();
        let Some(job) = &mut self.checking else {
            return false;
        };
        let checked = job
            .checked
            .lock()
            .map(|mut checked| mem::take(&mut *checked))
            .unwrap_or_default();
        if !checked.is_empty() {
            let comparison = &mut self.comparison;
            let mut started_differing = Vec::new();
            for (node, verdict) in checked {
                match verdict {
                    Ok(content) => {
                        job.compared += 1;
                        job.changed += usize::from(content == Content::Changed);
                        started_differing.extend(comparison.diff.set_content(node, content));
                    }
                    Err(error) => {
                        job.failed += 1;
                        job.first_error.get_or_insert_with(|| format!("{error:#}"));
                    }
                }
            }
            comparison
                .outline
                .grow(&comparison.diff, &started_differing);
            comparison.summary = summary(&comparison.diff, true);
            self.reselect(active);
        }
        cx.notify();
        true
    }

    fn finish_check(&mut self, cx: &mut Context<Self>) {
        self.take_checked(cx);
        let Some(job) = self.checking.take() else {
            return;
        };
        let cancelled = job.progress.is_cancelled();
        tracing::info!(
            compared = job.compared,
            changed = job.changed,
            failed = job.failed,
            cancelled,
            "contents compared"
        );
        let mut notice = format!("Compared {} files, {} changed", job.compared, job.changed);
        if cancelled {
            notice += ", stopped";
        }
        if let Some(first) = &job.first_error {
            notice += &format!(", {} failed, first: {first}", job.failed);
        }
        self.notice = Some(notice.into());
        cx.notify();
    }

    fn confirm_copy(&mut self, cx: &mut Context<Self>) {
        let Some(Copy::Confirm { plan, .. }) = self.copy.take() else {
            return;
        };
        tracing::info!(
            action = ?plan.action,
            files = plan.files,
            dirs = plan.dirs,
            bytes = plan.bytes,
            "copying"
        );
        let progress = Arc::new(JobProgress::default());
        let (action, ops, bytes) = (plan.action, plan.ops.len(), plan.bytes);
        let run = cx.spawn({
            let progress = progress.clone();
            async move |this, cx| {
                let report = cx
                    .background_executor()
                    .spawn(async move { sync::execute(&plan, &progress) })
                    .await;
                _ = this.update(cx, |this, cx| this.finish_copy(action, &report, cx));
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
            action,
            ops,
            bytes,
            progress,
            _run: run,
            _refresh: refresh,
        }));
        cx.notify();
    }

    fn finish_copy(&mut self, action: Action, report: &Report, cx: &mut Context<Self>) {
        self.copy = None;
        tracing::info!(
            ?action,
            copied = report.copied.len(),
            failed = report.failed.len(),
            removed = report.removed.len(),
            skipped = report.skipped,
            cancelled = report.cancelled,
            "copy finished"
        );
        let active = self.active_node();
        let comparison = &mut self.comparison;
        comparison.diff.mark_copied(&report.copied);
        if !report.removed.is_empty() {
            comparison
                .diff
                .mark_removed(&report.removed, action.target());
        }
        comparison.outline.refresh(&comparison.diff);
        comparison.summary = summary(&comparison.diff, true);
        self.keep_active(active);
        self.notice = Some(copy_notice(action, report).into());
        cx.notify();
    }

    /// Dismisses a pending confirmation, or stops the running copy or content check after
    /// the entry it is working on.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        match (&self.copy, &self.checking) {
            (Some(Copy::Confirm { .. }), _) => self.copy = None,
            (Some(Copy::Running(job)), _) => job.progress.cancel(),
            (None, Some(job)) => job.progress.cancel(),
            (None, None) => return,
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
                free.as_ref().map_or(true, |&free| plan.space <= free)
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
        let card = match self.copy.as_ref()? {
            Copy::Confirm { plan, free } => {
                let fits = self.copy_fits();
                let target = plan.action.target();
                let (from, to) = self.titles(target);
                let (question, confirm) = match plan.action {
                    Action::Copy { .. } => (
                        format!(
                            "Copy {} to the {}?",
                            describe(plan.files, plan.dirs, plan.bytes),
                            side_name(target)
                        ),
                        "Copy",
                    ),
                    Action::Overwrite => (
                        format!(
                            "Overwrite {} on the {} with the version from the {}?",
                            describe(plan.files, plan.dirs, plan.bytes),
                            side_name(target),
                            side_name(target.other())
                        ),
                        "Overwrite",
                    ),
                };
                let free = match free {
                    Ok(free) if fits => {
                        div().child(format!("Free on the target: {}", format_bytes(*free)))
                    }
                    Ok(free) => div().text_color(*theme::ERROR).child(format!(
                        "Not enough space: {} free on the target, {} needed",
                        format_bytes(*free),
                        format_bytes(plan.space)
                    )),
                    Err(error) => div()
                        .text_color(*theme::TEXT_MUTED)
                        .child(format!("Free space unknown: {error}")),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(question)
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
                                cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)),
                            ))
                            .child(button("copy-confirm", true, fits).child(confirm).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if this.copy_fits() {
                                        this.confirm_copy(cx);
                                    }
                                }),
                            )),
                    )
            }
            Copy::Running(job) => {
                let done = job.progress.done();
                let fraction = fraction(&job.progress, done, job.ops, job.bytes);
                let target = side_name(job.action.target());
                let title = match job.action {
                    Action::Copy { .. } => format!("Copying to the {target}…"),
                    Action::Overwrite => format!("Overwriting on the {target}…"),
                };
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(title)
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
                    .child(
                        button("copy-stop", false, true)
                            .child("Stop")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx))),
                    )
            }
        };
        Some(modal(card))
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
                )
            });
        }
        let job = self.checking.as_ref()?;
        let done = job.progress.done();
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
            )
            .child(
                div().flex().justify_end().child(
                    button("check-stop", false, true)
                        .child("Stop")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx))),
                ),
            ),
        )
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
        (None, Presence::Both) if node.content == Some(Content::Changed) => {
            cell.bg(*theme::CHANGED)
        }
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

/// A progress card in the bottom right corner of the list area, which must be `relative`.
/// Covers only itself, so the rows stay usable.
fn toast(title: &'static str, fraction: f32, caption: String) -> Div {
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
        .child(title)
        .child(progress_bar(fraction))
        .child(
            div()
                .max_w(px(320.))
                .truncate()
                .text_color(*theme::TEXT_MUTED)
                .child(caption),
        )
}

/// Share of a job done: by bytes when it has any, else by entries.
fn fraction(progress: &JobProgress, done: usize, total: usize, bytes: u64) -> f32 {
    if bytes > 0 {
        progress.bytes() as f32 / bytes as f32
    } else {
        done as f32 / total.max(1) as f32
    }
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

fn copy_notice(action: Action, report: &Report) -> String {
    let done = report.copied.len();
    let mut notice = match action {
        Action::Copy { .. } => format!("Copied {done} entries"),
        Action::Overwrite => format!("Overwrote {done} files"),
    };
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
/// every directory could be read; before that the counts are marked provisional. Files
/// whose bytes were not compared are listed apart, as their equal sizes prove nothing.
fn summary(diff: &TreeDiff, complete: bool) -> SharedString {
    let counts = diff.counts();
    let Missing { left, right } = counts.missing;
    let changed = counts.changed;
    let same = left == 0 && right == 0 && changed == 0;
    let mut summary = match (complete, same) {
        (true, true) if diff.errors.is_empty() => "Same entries on both sides".to_owned(),
        (true, true) => "No differences found among readable entries".to_owned(),
        (true, false) => format!(
            "Missing on the left: {left} · Missing on the right: {right} · Changed: {changed}"
        ),
        (false, true) => "Comparing… no differences so far".to_owned(),
        (false, false) => format!(
            "Comparing… so far missing on the left: {left} · on the right: {right} · \
             changed: {changed}"
        ),
    };
    if complete && counts.unchecked > 0 {
        summary += &format!(" · Contents not compared: {}", counts.unchecked);
    }
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
        let toast = self.toast(cx);
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
