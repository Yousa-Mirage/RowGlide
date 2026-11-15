//! Independent clipboard task lifecycle; query status and execution stay untouched.

use crate::i18n::tr;

use super::Workspace;
use crate::clipboard::CopyRequested;
use gpui_kit::*;
use rowglide_core::{Cancellation, CopyError, CopyLimits, serialize_cell};
use std::time::Duration;

/// At most one clipboard request may publish; a newer request cancels its predecessor.
#[derive(Default)]
pub(super) struct ClipboardState {
    /// Cancels serialization and cache waiting without cancelling the underlying SQL result.
    cancel: Cancellation,
    /// Owns the completion future; replacing it suppresses stale clipboard writes.
    task: Option<Task<()>>,
    /// Independent status feedback, including explicit size-limit and read failures.
    pub(super) message: Option<String>,
}

impl Drop for ClipboardState {
    /// Stop background serialization when the workspace closes.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Workspace {
    /// Resolve the captured cell asynchronously and serialize on a background executor.
    /// Only successful complete output reaches the clipboard. Missing SQL batches are
    /// awaited for up to 30 seconds through an independent reply, with no polling.
    /// Superseding requests cancel the previous worker and completion task.
    pub(super) fn copy_cell(&mut self, request: CopyRequested, cx: &mut Context<Self>) {
        self.clipboard.cancel.cancel();
        self.clipboard.task = None;
        self.clipboard.cancel = Cancellation::default();
        self.clipboard.message = Some(tr!("copy.loading"));
        let cancel = self.clipboard.cancel.clone();
        self.clipboard.task = Some(cx.spawn(async move |this, cx| {
            let resolved = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(CopyError::from(rowglide_core::QueryError::Cancelled)),
                result = request.target.resolve(&cancel) => result,
                _ = cx.background_executor().timer(Duration::from_secs(30)) => {
                    Err(CopyError::Invalid(tr!("copy.timeout")))
                }
            };
            let outcome = match resolved {
                Ok((array, row)) => {
                    let worker_cancel = cancel.clone();
                    cx.background_executor()
                        .spawn(async move {
                            serialize_cell(
                                array.as_ref(),
                                row,
                                request.format,
                                CopyLimits::default(),
                                &worker_cancel,
                            )
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if cancel.check().is_err() {
                    return;
                }
                this.clipboard.message = Some(match outcome {
                    Ok(text) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        tr!("copy.success")
                    }
                    Err(error) => tr!("copy.failed", error = error),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }
}
