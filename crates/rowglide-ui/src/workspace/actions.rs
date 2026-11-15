//! Scoped keyboard commands and focus restoration for the workspace.

use super::{TabMode, Workspace};
use crate::clipboard::CopyRequested;
use gpui_kit::base::actions::{SelectDown, SelectNextColumn, SelectPrevColumn, SelectUp};
use gpui_kit::*;
use rowglide_core::CopyFormat;

/// Copy the focused data grid's selected cell without its display truncation.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct CopyCell;
/// Copy the focused data grid's selected cell using JSON serialization.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct CopyJson;
/// Open the focused data grid's selected cell in the inspector.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct InspectCell;
/// Focus the Browse filter without applying its draft or changing modes.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct Find;
/// Execute the SQL draft when the active file is in SQL mode.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct RunSql;
/// Close the active file while leaving the application window open.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct CloseTab;
/// Dismiss one workspace panel or selection after focused popups have handled Escape.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
pub(super) struct Dismiss;

/// Install bindings after component initialization, using Cmd on macOS and Ctrl elsewhere.
pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            RunSql,
            Some("RowglideSql > Input"),
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            },
            CopyCell,
            Some("RowglideGrid > DataTable"),
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-shift-c"
            } else {
                "ctrl-shift-c"
            },
            CopyJson,
            Some("RowglideGrid > DataTable"),
        ),
        KeyBinding::new("enter", InspectCell, Some("RowglideGrid > DataTable")),
        KeyBinding::new("space", InspectCell, Some("RowglideGrid > DataTable")),
        KeyBinding::new("escape", Dismiss, Some("RowglideGrid > DataTable")),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-f"
            } else {
                "ctrl-f"
            },
            Find,
            Some("Rowglide"),
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            RunSql,
            Some("Rowglide"),
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-w"
            } else {
                "ctrl-w"
            },
            CloseTab,
            Some("Rowglide"),
        ),
        KeyBinding::new("escape", Dismiss, Some("Rowglide")),
    ]);
}

impl Workspace {
    /// Attach workspace commands to both the empty view and populated layout.
    pub(super) fn keyboard_root(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("workspace")
            .key_context("Rowglide")
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                if !this.tabs.is_empty() && this.active_tab().mode == TabMode::Browse {
                    this.search.focus_handle(cx).focus(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &RunSql, window, cx| {
                if !this.tabs.is_empty() && this.active_tab().mode == TabMode::Sql {
                    this.run_sql(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(tab) = this.tabs.get(this.active_tab) {
                    this.close_tab(tab.source.id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &CopyCell, window, cx| {
                this.copy_selected(CopyFormat::Value, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyJson, window, cx| {
                this.copy_selected(CopyFormat::Json, window, cx)
            }))
            .on_action(cx.listener(|this, _: &InspectCell, window, cx| {
                if this.table.focus_handle(cx).is_focused(window)
                    && let Some((row, col)) = this.table.read(cx).selected_cell()
                {
                    this.inspect_cell(row, col, cx);
                }
            }))
    }

    /// Enter cell selection on the first arrow press; subsequent movement stays with TableState.
    /// The upstream table defaults back to row selection after clearing or replacing results.
    fn start_cell_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.table.focus_handle(cx).is_focused(window)
            && self.table.read(cx).selected_cell().is_none()
            && self.table.read(cx).delegate().copy_target(0, 0).is_some()
        {
            self.table
                .update(cx, |table, cx| table.set_selected_cell(0, 0, cx));
            cx.stop_propagation();
        }
    }

    /// Scope the main grid and initialize cell navigation before upstream arrow handlers run.
    pub(super) fn keyboard_grid(&self, cx: &mut Context<Self>) -> Div {
        div()
            .key_context("RowglideGrid")
            .capture_action(cx.listener(|this, _: &SelectDown, window, cx| {
                this.start_cell_navigation(window, cx)
            }))
            .capture_action(
                cx.listener(|this, _: &SelectUp, window, cx| {
                    this.start_cell_navigation(window, cx)
                }),
            )
            .capture_action(cx.listener(|this, _: &SelectNextColumn, window, cx| {
                this.start_cell_navigation(window, cx)
            }))
            .capture_action(cx.listener(|this, _: &SelectPrevColumn, window, cx| {
                this.start_cell_navigation(window, cx)
            }))
    }

    /// Capture an immutable typed copy target only while the main table owns focus.
    fn copy_selected(&mut self, format: CopyFormat, window: &mut Window, cx: &mut Context<Self>) {
        if !self.table.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        let table = self.table.read(cx);
        let target = table
            .selected_cell()
            .and_then(|(row, col)| table.delegate().copy_target(row, col));
        if let Some(target) = target {
            self.copy_cell(CopyRequested { target, format }, cx);
        }
    }

    /// Open a valid cell, retaining Arrow buffers or requesting an asynchronous SQL cache read.
    pub(super) fn inspect_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if self
            .table
            .read(cx)
            .delegate()
            .copy_target(row, col)
            .is_none()
        {
            return;
        }
        self.close_file_info(cx);
        self.selected = self
            .table
            .read(cx)
            .delegate()
            .row(row)
            .map(|(batch, local)| {
                (
                    batch.schema().field(col).name().clone(),
                    batch.column(col).clone(),
                    local,
                )
            });
        self.pending_cell = self.selected.is_none().then_some((row, col));
        self.expanded.clear();
        self.expanded.insert("value".into());
        cx.notify();
    }

    /// Restore focus to the visible data grid, or the Open files button after the last tab closes.
    pub(super) fn focus_content(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            self.open_focus.focus(window, cx);
        } else {
            self.table.focus_handle(cx).focus(window, cx);
        }
    }

    /// Close exactly one detail panel, otherwise clear the main cell selection.
    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() && self.active_tab().info.open {
            self.close_file_info(cx);
        } else if self.selected.is_some() || self.pending_cell.is_some() {
            self.selected = None;
            self.pending_cell = None;
        } else if self.table.read(cx).selected_cell().is_some() {
            self.table.update(cx, |table, cx| table.clear_selection(cx));
        } else {
            cx.propagate();
            return;
        }
        self.focus_content(window, cx);
        cx.notify();
    }
}
