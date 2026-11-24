mod check;
mod copy;
mod text;
mod view;

use std::{
    mem,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui_kit::{
    App, Context, FocusHandle, Focusable, ScrollStrategy, SharedString, Task,
    UniformListScrollHandle, Window, actions,
};

use check::CheckJob;
use copy::Copy;
use text::summary;

use super::list::{Cursor, Move};
use crate::{
    diff::{self, Found, Outline, Progress, TreeDiff},
    storage::location::Location,
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
    /// A copy or overwrite, shown in a dialog while it is confirmed and runs.
    copy: Option<Copy>,
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
                        diff::walk(&left, &right, &progress, &mut |item| {
                            if let Ok(mut found) = found.lock() {
                                found.push(item);
                            }
                        });
                    })
                    .await;
                _ = this.update(cx, |this, cx| this.finish(cx));
            }
        });
        // Redraws on every tick, not only when entries were found: the toast has to appear
        // once [`PROGRESS_DELAY`] passes, and its progress moves while a large directory
        // is read without yielding new entries.
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

    /// Stops the running copy or content check after the entry it is working on. A pending
    /// confirmation is its dialog's to dismiss.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let progress = match (&self.copy, &self.checking) {
            (Some(Copy::Running(job)), _) => &job.progress,
            (None, Some(job)) => &job.progress,
            _ => return,
        };
        progress.cancel();
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

    /// Expands or collapses the active directory.
    fn toggle(&mut self, cx: &mut Context<Self>) {
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
}

impl Focusable for TreeCompare {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
