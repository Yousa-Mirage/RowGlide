//! Independent clipboard task lifecycle; query status and execution stay untouched.

use crate::i18n::tr_string;

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
    /// Resolve the captured cell or rectangle and serialize on a background executor.
    /// Only successful complete output reaches the clipboard. Missing SQL batches are
    /// awaited for up to 30 seconds through an independent reply, with no polling.
    /// Superseding requests cancel the previous worker and completion task.
    pub(super) fn copy_cell(&mut self, request: CopyRequested, cx: &mut Context<Self>) {
        self.clipboard.cancel.cancel();
        self.clipboard.task = None;
        self.clipboard.cancel = Cancellation::default();
        self.clipboard.message = Some(tr_string!("copy.loading"));
        let cancel = self.clipboard.cancel.clone();
        self.clipboard.task = Some(cx.spawn(async move |this, cx| {
            let worker_cancel = cancel.clone();
            let worker = cx.background_executor().spawn(async move {
                match request {
                    CopyRequested::Cell { target, format } => {
                        let (array, row) = target.resolve(&worker_cancel).await?;
                        serialize_cell(
                            array.as_ref(),
                            row,
                            format,
                            CopyLimits::default(),
                            &worker_cancel,
                        )
                    }
                    CopyRequested::Range(range, format) => {
                        range.serialize(format, &worker_cancel).await
                    }
                }
            });
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = worker => Some(result),
                _ = cx.background_executor().timer(Duration::from_secs(30)) => None,
            };
            let _ = this.update(cx, |this, cx| {
                if cancel.check().is_err() {
                    return;
                }
                let outcome = outcome.unwrap_or_else(|| {
                    cancel.cancel();
                    Err(CopyError::Invalid(tr_string!("copy.timeout")))
                });
                this.clipboard.message = Some(match outcome {
                    Ok(text) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        tr_string!("copy.success")
                    }
                    Err(error) => tr_string!("copy.failed", error = error),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }
}
