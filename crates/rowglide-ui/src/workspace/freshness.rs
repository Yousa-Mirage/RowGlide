//! Low-frequency metadata checks and explicit, transactional source reloads.

#[cfg(test)]
mod tests;

use super::{Workspace, state::TabState};
use crate::i18n::tr;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt, button::Button, notification::Notification,
};
use gpui_kit::*;
use rowglide_core::Cancellation;
use std::time::Duration;

/// Stable notification identity for a failed explicit reload.
struct ReloadFailure;

impl Workspace {
    /// Check all live, unchanged sources serially every three seconds off the GUI thread.
    /// No engine permit, file reads or per-tab task is required. Revision checks discard
    /// samples that overlap reload/closure. Dropping the workspace stops future polls.
    pub(super) fn watch_sources(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let Ok((backend, sources, revision)) = this.update(cx, |this, _| {
                    (
                        this.backend.clone(),
                        this.tabs
                            .iter()
                            .filter(|tab| !tab.changed)
                            .map(|tab| tab.source.id)
                            .collect::<Vec<_>>(),
                        this.source_revision,
                    )
                }) else {
                    break;
                };
                let changed = cx
                    .background_executor()
                    .spawn(async move {
                        sources
                            .into_iter()
                            .filter(|id| {
                                // A failed check also means freshness cannot be guaranteed.
                                backend.source_changed(*id).unwrap_or(true)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.source_revision == revision {
                            this.mark_changed(&changed, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Latch warnings per source and cancel active reads while preserving retained data.
    /// Closed IDs and repeated samples do nothing; inactive tabs keep their own warning.
    pub(super) fn mark_changed(&mut self, sources: &[usize], cx: &mut Context<Self>) {
        let mut active_changed = false;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if sources.contains(&tab.source.id) && !tab.changed {
                tab.changed = true;
                active_changed |= index == self.active_tab;
            }
        }
        if active_changed {
            if self.busy {
                self.cancel(cx);
            }
            self.close_file_info(cx);
        }
        if !sources.is_empty() {
            cx.notify();
        }
    }

    /// Display a persistent, nonmodal freshness warning independent of query/clipboard status.
    pub(super) fn render_source_warning(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_4()
            .py_1()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_3()
            .bg(cx.theme().muted)
            .text_color(cx.theme().foreground)
            .text_sm()
            .child(div().flex_1().child(tr!("files.changed")))
            .child(
                Button::new("reload-source")
                    .small()
                    .label(if self.reloading.is_some() {
                        tr!("files.reloading")
                    } else {
                        tr!("files.reload")
                    })
                    .disabled(self.reloading.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.reload_source(window, cx))),
            )
    }

    /// Reinspect the active source in the background and replace its sessions only on success.
    /// Failure preserves old results and the warning. Successful reload starts a fresh Browse
    /// page and keeps the SQL draft, but never automatically reruns user SQL. Closed tabs and
    /// cancelled reloads cannot be recreated by a late completion.
    pub(super) fn reload_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() || self.reloading.is_some() {
            return;
        }
        window.remove_notification::<ReloadFailure>(cx);
        self.save_session(cx);
        self.close_file_info(cx);
        let source = self.active_tab().source.id;
        self.reloading = Some(source);
        self.source_revision += 1;
        self.reload_cancel = Cancellation::default();
        let cancel = self.reload_cancel.clone();
        let backend = self.backend.clone();
        self.reload_task = Some(cx.spawn_in(window, async move |this, cx| {
            let worker_cancel = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move { backend.reload(source, worker_cancel) })
                .await;
            if cancel.check().is_err() {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.source_revision += 1;
                this.reloading = None;
                let Some(index) = this.tabs.iter().position(|tab| tab.source.id == source) else {
                    return;
                };
                match result {
                    Ok(source) => {
                        // Capture edits made while inspection ran, including the active SQL draft.
                        if index == this.active_tab {
                            this.save_session(cx);
                        }
                        let sql = this.tabs[index].sql.clone();
                        this.tabs[index] = TabState::new(source);
                        this.tabs[index].sql = sql;
                        if index == this.active_tab {
                            this.restore_session(window, cx);
                            this.focus_content(window, cx);
                        }
                    }
                    Err(error) => {
                        this.tabs[index].changed = true;
                        window.push_notification(
                            Notification::error(crate::i18n::query_error(&error))
                                .id::<ReloadFailure>()
                                .autohide(false),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
        // Move the saved snapshot back to the table while inspection runs.
        self.restore_session(window, cx);
        cx.notify();
    }
}
