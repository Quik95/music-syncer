//! Comparing the bytes of equal-sized files in the background.

use std::{
    mem,
    sync::{Arc, Mutex},
};

use gpui_kit::{Context, Task, Window};

use super::{PROGRESS_REFRESH, Scope, TreeCompare, text::summary, view::push_outcome};
use crate::{
    diff::{
        Content,
        content::{self, CheckPlan, Verdict},
    },
    progress::JobProgress,
};

/// File contents being compared in the background; verdicts show up as they come.
/// Dropping it cancels it after the file being compared.
pub(super) struct CheckJob {
    pub(super) files: usize,
    pub(super) bytes: u64,
    pub(super) progress: Arc<JobProgress>,
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
    /// Whether [`Self::check_contents`] would find files to compare right now.
    pub fn can_check(&self, scope: Scope) -> bool {
        self.idle()
            && self
                .scope_node(scope)
                .is_ok_and(|node| content::has_work(&self.comparison.diff, node))
    }

    /// Compares the bytes of the files in `scope` that are on both sides with equal sizes,
    /// in the background.
    pub fn check_contents(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
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
        let run = cx.spawn_in(window, {
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
                _ = this.update_in(cx, |this, window, cx| this.finish_check(window, cx));
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

    fn finish_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let complete = !cancelled && job.failed == 0;
        push_outcome("Contents compared", notice, complete, window, cx);
        cx.notify();
    }
}
