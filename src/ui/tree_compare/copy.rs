//! Copying and overwriting entries: confirmation, the job in the background, and its
//! dialogs.

use std::sync::Arc;

use gpui_kit::{
    App, Context, Entity, SharedString, Task, Window,
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::{Dialog, DialogEntrance, DialogFooter},
        progress::Progress as ProgressBar,
        v_flex,
    },
    div,
    prelude::*,
};

use super::{
    PROGRESS_REFRESH, Scope, TreeCompare,
    text::{describe, format_bytes, side_name, summary},
    view::{fraction, push_outcome},
};
use crate::{
    diff::Side,
    progress::JobProgress,
    sync::{self, Action, Plan, Report},
};

/// A copy or overwrite.
pub(super) enum Copy {
    /// Waiting for the user to confirm; `free` is the space left on the target.
    Confirm {
        plan: Plan,
        free: Result<u64, String>,
    },
    Running(CopyJob),
}

/// A copy or overwrite running in the background. Dropping it, e.g. by closing the
/// comparison, cancels it after the entry being copied.
pub(super) struct CopyJob {
    action: Action,
    ops: usize,
    bytes: u64,
    pub(super) progress: Arc<JobProgress>,
    _run: Task<()>,
    _refresh: Task<()>,
}

impl Drop for CopyJob {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

impl TreeCompare {
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
        // The dialog hands focus back to what had it, so the rows rather than the button
        // that asked for the copy.
        window.focus(&self.focus_handle, cx);
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            view.read(cx).copy_dialog(dialog, &view, cx)
        });
        cx.notify();
    }

    /// Starts the pending copy when it fits into the target's free space.
    fn confirm_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.copy_fits() {
            return;
        }
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
        let run = cx.spawn_in(window, {
            let progress = progress.clone();
            async move |this, cx| {
                let report = cx
                    .background_executor()
                    .spawn(async move { sync::execute(&plan, &progress) })
                    .await;
                _ = this.update_in(cx, |this, window, cx| {
                    this.finish_copy(action, &report, window, cx);
                });
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

    fn finish_copy(
        &mut self,
        action: Action,
        report: &Report,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy = None;
        window.close_dialog(cx);
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
        let title = match action {
            Action::Copy { .. } => "Copy finished",
            Action::Overwrite => "Overwrite finished",
        };
        let complete = !report.cancelled && report.failed.is_empty();
        push_outcome(title, copy_notice(action, report), complete, window, cx);
        cx.notify();
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

    /// The pending copy's confirmation, then its progress while it runs. Built by the
    /// window's dialog layer on every render, so it follows [`Self::copy`].
    ///
    /// Without animation: the layer keeps building a dialog through its exit, by when
    /// [`Self::copy`] is already gone, so an animated exit would show an empty dialog.
    fn copy_dialog(&self, dialog: Dialog, view: &Entity<Self>, cx: &App) -> Dialog {
        let dialog = dialog.entrance(DialogEntrance::None);
        match &self.copy {
            None => dialog,
            Some(Copy::Confirm { plan, free }) => self.confirm_dialog(dialog, plan, free, view, cx),
            Some(Copy::Running(job)) => running_dialog(dialog, job, view, cx),
        }
    }

    /// What the pending copy takes and whether it fits, with buttons to run or drop it.
    fn confirm_dialog(
        &self,
        dialog: Dialog,
        plan: &Plan,
        free: &Result<u64, String>,
        view: &Entity<Self>,
        cx: &App,
    ) -> Dialog {
        let theme = cx.theme();
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
            Ok(free) if fits => div().child(format!("Free on the target: {}", format_bytes(*free))),
            Ok(free) => div().text_color(theme.danger).child(format!(
                "Not enough space: {} free on the target, {} needed",
                format_bytes(*free),
                format_bytes(plan.space)
            )),
            Err(error) => div()
                .text_color(theme.muted_foreground)
                .child(format!("Free space unknown: {error}")),
        };
        let footer = DialogFooter::new()
            .child(Button::new("copy-cancel").label("Cancel").on_click({
                let view = view.clone();
                move |_, window, cx| {
                    view.update(cx, Self::dismiss_copy);
                    window.close_dialog(cx);
                }
            }))
            .child(
                Button::new("copy-confirm")
                    .primary()
                    .label(confirm)
                    .disabled(!fits)
                    .on_click({
                        let view = view.clone();
                        move |_, window, cx| {
                            view.update(cx, |this, cx| this.confirm_copy(window, cx));
                        }
                    }),
            );
        dialog
            .title(confirm)
            .child(
                v_flex()
                    .gap_2()
                    .child(question)
                    .child(
                        v_flex()
                            .text_color(theme.muted_foreground)
                            .child(format!("From {from}"))
                            .child(format!("To {to}")),
                    )
                    .child(free),
            )
            .footer(footer)
            // Enter, which keeps the dialog open as it turns into the progress.
            .on_ok({
                let view = view.clone();
                move |_, window, cx| {
                    view.update(cx, |this, cx| this.confirm_copy(window, cx));
                    false
                }
            })
            // Escape, the close button or a click beside the dialog; closing it through
            // the window skips this, see the Cancel button.
            .on_close({
                let view = view.clone();
                move |_, _, cx| view.update(cx, Self::dismiss_copy)
            })
    }

    /// Forgets a copy that was not confirmed when its dialog closes.
    fn dismiss_copy(&mut self, cx: &mut Context<Self>) {
        if matches!(self.copy, Some(Copy::Confirm { .. })) {
            self.copy = None;
            cx.notify();
        }
    }

    /// Column titles of the side opposite to `target` and of `target`.
    fn titles(&self, target: Side) -> (&SharedString, &SharedString) {
        match target {
            Side::Left => (&self.right_title, &self.left_title),
            Side::Right => (&self.left_title, &self.right_title),
        }
    }
}

/// Progress of a running copy with a button to stop it. Escape stops it too; the dialog
/// stays until the copy ends, so neither Escape nor Enter closes it.
fn running_dialog(dialog: Dialog, job: &CopyJob, view: &Entity<TreeCompare>, cx: &App) -> Dialog {
    let done = job.progress.done();
    let fraction = fraction(&job.progress, done, job.ops, job.bytes);
    let target = side_name(job.action.target());
    let title = match job.action {
        Action::Copy { .. } => format!("Copying to the {target}…"),
        Action::Overwrite => format!("Overwriting on the {target}…"),
    };
    let stop = Button::new("copy-stop").label("Stop").on_click({
        let view = view.clone();
        move |_, _, cx| view.update(cx, TreeCompare::cancel)
    });
    dialog
        .title(title)
        .close_button(false)
        .overlay_closable(false)
        .on_cancel({
            let view = view.clone();
            move |_, _, cx| {
                view.update(cx, TreeCompare::cancel);
                false
            }
        })
        .on_ok(|_, _, _| false)
        .child(
            v_flex()
                .gap_2()
                .child(ProgressBar::new("copy-progress").value(fraction * 100.))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(format!(
                            "{} of {} · {}",
                            (done + 1).min(job.ops),
                            job.ops,
                            job.progress.current()
                        )),
                ),
        )
        .footer(DialogFooter::new().child(stop))
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
