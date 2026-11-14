//! Native file selection and background schema inspection.

use super::{Workspace, state::TabState};
use gpui_kit::component::{Sizable, button::Button};
use gpui_kit::*;
use rowglide_core::Cancellation;

impl Workspace {
    /// Close one source tab, cancelling active work and releasing its retained results.
    /// Closing an inactive tab preserves the current query and drafts. The active tab
    /// falls forward to its right neighbor, then left, or back to the empty view.
    /// Independent clipboard work may retain its captured value until copying finishes.
    pub(super) fn close_tab(&mut self, source: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.source.id == source) else {
            return;
        };
        if let Err(error) = self.backend.close(source) {
            self.status = format!("Cannot close file: {error}");
            self.error = true;
            cx.notify();
            return;
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
            self.query = Default::default();
            self.sql_open = false;
            self.error = false;
            self.status = "Ready".into();
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
        cx.notify();
    }

    /// Build the shared Open files button and wire it to native path selection.
    pub(super) fn render_open_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("open-files")
            .small()
            .label("Open files…")
            .on_click(cx.listener(|this, _, window, cx| this.open_files(window, cx)))
    }

    /// Choose local files, inspect them in background work and activate the last opened tab.
    ///
    /// A new invocation cancels the previous inspection token. Successful duplicate
    /// sources reuse their tabs; failures are collected into the status message.
    /// Cancelling the picker leaves the workspace unchanged. Tab switches save the
    /// current session and cancel its query through the normal session lifecycle.
    fn open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.opening_cancel.cancel();
        self.opening_cancel = Cancellation::default();
        let cancel = self.opening_cancel.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open CSV or Parquet files".into()),
        });
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let paths = match paths.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                error => {
                    let _ = this.update(cx, |this, cx| {
                        this.status = format!("Cannot open file picker: {error:?}");
                        this.error = true;
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.tabs.is_empty() {
                    this.status = "Inspecting files…".into();
                }
                cx.notify();
            });
            let worker_cancel = cancel.clone();
            let results = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let result = backend.open(&path, worker_cancel.clone());
                            (path, result)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            if cancel.check().is_err() {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
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
                        Err(error) => errors.push(format!("{}: {error}", path.display())),
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
                }
                if !errors.is_empty() {
                    this.status = errors.join("; ");
                    this.error = true;
                }
                cx.notify();
            });
        })
        .detach();
    }
}
