//! Native file selection, external drops, startup paths and shared background inspection.

#[cfg(test)]
mod tests;

use crate::i18n::{tr, tr_string};

use super::{Workspace, state::TabState};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt, button::Button, notification::Notification,
};
use gpui_kit::*;
use rowglide_core::Cancellation;
use std::path::PathBuf;

/// Identifies the latest file-opening failure notification independently of query status.
struct OpenFailure;

impl Workspace {
    /// Forward OS file paths to the shared asynchronous opener without inspecting the filesystem.
    /// Empty payloads are harmless; backend validation reports directories and unsupported files.
    /// GPUI invokes this only when an external file drag is submitted inside the workspace.
    pub(super) fn drop_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_paths(paths.paths().to_vec(), window, cx);
    }

    /// Render a layout-neutral hint only while OS file paths hover over the workspace.
    /// GPUI owns drag lifetime, so leaving or ending a drag clears this without retained UI state.
    /// Internal divider drags use another payload type and never activate this overlay.
    pub(super) fn render_file_drop_hint(&self, cx: &App) -> impl IntoElement {
        div()
            .id("file-drop-hint")
            .test_support()
            .absolute()
            .inset_0()
            .border_2()
            .border_color(cx.theme().ring)
            .opacity(0.)
            .drag_over::<ExternalPaths>(|style, _, _, _| style.opacity(1.))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(cx.theme().popover)
                    .text_color(cx.theme().popover_foreground)
                    .child(tr!("files.drop")),
            )
    }

    /// Close one source tab, cancelling active work and releasing its retained results.
    /// Closing an inactive tab preserves the current query and drafts. The active tab
    /// falls forward to its right neighbor, then left, or back to the empty view.
    /// Independent clipboard work may retain its captured value until copying finishes.
    pub(super) fn close_tab(&mut self, source: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.source.id == source) else {
            return;
        };
        if let Err(error) = self.backend.close(source) {
            self.status = tr_string!(
                "errors.close_file",
                error = crate::i18n::query_error(&error)
            );
            self.error = true;
            cx.notify();
            return;
        }

        self.source_revision += 1;
        if self.reloading == Some(source) {
            self.reload_cancel.cancel();
            self.reload_task = None;
            self.reloading = None;
        }
        let was_active = index == self.active_tab;
        if was_active {
            self.sizing_task = None;
            self.cancellation.cancel();
            self.task = None;
            self.generation += 1;
            self.busy = false;
            self.close_file_info(cx);
            self.selected = None;
            self.pending_cell = None;
            self.expanded.clear();
            self.table.update(cx, |table, cx| {
                table.clear_selection(cx);
                let grid = table.delegate_mut();
                grid.visible = None;
                grid.view_id += 1;
                grid.widths = None;
                grid.preview = None;
                grid.sql_result = None;
                grid.sql_snapshot = None;
                grid.loading = false;
                grid.sort = None;
                table.refresh(cx);
            });
        }

        super::state::remove_tab(&mut self.tabs, &mut self.active_tab, index);
        if self.tabs.is_empty() {
            self.filter_editors.clear();
            self.query = Default::default();
            self.sql_open = false;
            self.error = false;
            self.status = tr_string!("common.ready");
            self.search
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.limit
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.offset
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.sql
                .update(cx, |state, cx| state.set_value("", window, cx));
        } else if was_active {
            self.restore_session(window, cx);
            self.refresh_file_info(cx);
        }
        if was_active {
            self.focus_content(window, cx);
        }
        cx.notify();
    }

    /// Build the shared Open files button and wire it to native path selection.
    pub(super) fn render_open_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("open-files")
            .track_focus(&self.open_focus)
            .small()
            .label(tr!("files.open"))
            .on_click(cx.listener(|this, _, window, cx| this.open_files(window, cx)))
    }

    /// Choose local files and pass accepted paths to the shared background opener.
    /// Opening a picker cancels earlier inspection; cancelled or superseded pickers
    /// do not change tabs. Picker failures are displayed only for the current request.
    pub(super) fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.opening_cancel.cancel();
        self.opening_cancel = Cancellation::default();
        let cancel = self.opening_cancel.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(tr!("files.prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selected = paths.await;
            if cancel.check().is_err() {
                return;
            }
            let paths = match selected {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                error => {
                    let _ = this.update(cx, |this, cx| {
                        this.status = tr_string!("errors.picker", error = format!("{error:?}"));
                        this.error = true;
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.open_paths(paths, window, cx);
            });
        })
        .detach();
    }

    /// Inspect picker, dropped or startup paths off the GUI thread, then activate the last success.
    ///
    /// Empty input is a no-op. New work cancels previous inspection and ignores stale
    /// completion. Canonical duplicate sources reuse tabs; per-file failures are
    /// collected for display without preventing other files from opening. Activation
    /// saves the current session and uses its normal query-cancellation lifecycle.
    /// Successful activation reveals the tab and focuses the grid. Failures remain in a
    /// dismissible notification so a sibling file's query cannot overwrite their report.
    pub(super) fn open_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.opening_cancel.cancel();
        self.opening_cancel = Cancellation::default();
        let cancel = self.opening_cancel.clone();
        let backend = self.backend.clone();
        let recent = self.recent_store.clone();
        if self.tabs.is_empty() {
            self.status = tr_string!("files.inspecting");
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let worker_cancel = cancel.clone();
            let (results, recent_files) = cx
                .background_executor()
                .spawn(async move {
                    let results = paths
                        .into_iter()
                        .map(|path| {
                            let result = backend.open(&path, worker_cancel.clone());
                            if result.is_ok()
                                && worker_cancel.check().is_ok()
                                && let Some(recent) = &recent
                                && let Err(error) = recent.record(&path)
                            {
                                eprintln!("{error}");
                            }
                            (path, result)
                        })
                        .collect::<Vec<_>>();
                    let entries = recent.and_then(|recent| match recent.entries() {
                        Ok(entries) => Some(entries),
                        Err(error) => {
                            eprintln!("{error}");
                            None
                        }
                    });
                    (results, entries)
                })
                .await;
            if cancel.check().is_err() {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.recent_revision += 1;
                if let Some(entries) = recent_files {
                    this.recent_files = entries;
                }
                let had_tabs = !this.tabs.is_empty();
                let mut opened = None;
                let mut errors = Vec::new();
                // A duplicate open may finish after its tab was explicitly closed.
                let registered = this.backend.sources();
                for (path, result) in results {
                    match result {
                        Ok(source) => {
                            if !registered.iter().any(|file| file.id == source.id) {
                                continue;
                            }
                            let index = if let Some(index) =
                                this.tabs.iter().position(|tab| tab.source.id == source.id)
                            {
                                index
                            } else {
                                this.tabs.push(TabState::new(source));
                                this.tabs.len() - 1
                            };
                            opened = Some(index);
                        }
                        Err(error) => errors.push(format!(
                            "{}: {}",
                            path.display(),
                            crate::i18n::query_error(&error)
                        )),
                    }
                }
                if let Some(index) = opened {
                    if !had_tabs {
                        this.active_tab = index;
                        this.restore_session(window, cx);
                    } else if index != this.active_tab {
                        this.switch_tab(index, window, cx);
                    } else if !this.active_tab().browse_result.initialized {
                        this.restore_session(window, cx);
                    }
                    this.tabs_scroll.scroll_to_item(index);
                    this.focus_content(window, cx);
                }
                if !errors.is_empty() {
                    let message = errors.join("; ");
                    if !this.busy {
                        this.status = message.clone();
                        this.error = true;
                    }
                    // Keep opening failures independent of an in-flight query's status.
                    window.push_notification(
                        Notification::error(message)
                            .id::<OpenFailure>()
                            .autohide(false),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }
}
