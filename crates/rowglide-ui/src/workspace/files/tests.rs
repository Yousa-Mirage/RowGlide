//! Native file-drop dispatch tests using a deterministic backend registry.

use super::super::Workspace;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, ExternalPaths, FileDropEvent, Focusable, InputEvent,
    TestAppContext, WindowOptions,
};
use rowglide_core::{
    Backend, BatchCursor, CacheLimits, Cancellation, MockBackend, Query, QueryError, ResultStore,
    Source,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Records attempted paths and exposes successful registrations without filesystem access.
#[derive(Default)]
struct OpeningBackend {
    /// Registered fixture sources, identified by their original path.
    files: Mutex<Vec<(PathBuf, Source)>>,
    /// Calls that passed cancellation, used to distinguish hovering from submitting.
    calls: Mutex<Vec<PathBuf>>,
}

impl Backend for OpeningBackend {
    /// Return a snapshot of fixture registrations, recovering poisoned test-only storage.
    fn sources(&self) -> Vec<Source> {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, source)| source.clone())
            .collect()
    }

    /// Register a named fixture, reuse duplicates and reject the designated invalid paths.
    fn open(&self, path: &Path, cancel: Cancellation) -> Result<Source, QueryError> {
        cancel.check()?;
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_owned());
        if path.extension().is_none() || path.file_name().is_some_and(|name| name == "bad.parquet")
        {
            return Err(QueryError::Invalid("Fixture cannot read this file".into()));
        }
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, source)) = files.iter().find(|(registered, _)| registered == path) {
            return Ok(source.clone());
        }
        let source = Source {
            id: files.len(),
            name: path.display().to_string(),
            format: "PARQUET",
            rows: Some(0),
            parquet: None,
            description: "Dropped fixture",
        };
        files.push((path.to_owned(), source.clone()));
        Ok(source)
    }

    /// Return the empty mock result so tests exercise the normal post-open query completion.
    fn query(
        &self,
        mut query: Query,
        cancel: Cancellation,
    ) -> Result<Box<dyn BatchCursor>, QueryError> {
        query.source = 2;
        MockBackend.query(query, cancel)
    }
}

/// Owns the headless window, actual workspace and observable backend for one drop scenario.
struct Fixture {
    /// Native-event dispatch destination.
    window: AnyWindowHandle,
    /// Production workspace under test.
    view: Entity<Workspace>,
    /// Deterministic file opener shared with the workspace's background tasks.
    backend: Arc<OpeningBackend>,
}

/// Create an empty production workspace with no startup files or pre-registered sources.
fn fixture(cx: &mut TestAppContext) -> Result<Fixture, Box<dyn std::error::Error>> {
    let results = ResultStore::new(CacheLimits::default())?;
    let backend = Arc::new(OpeningBackend::default());
    let (window, view) = cx.update(|cx| {
        gpui_kit::init(cx);
        super::super::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(backend.clone(), results, Vec::new(), window, cx))
        })
    })?;
    Ok(Fixture {
        window,
        view,
        backend,
    })
}

/// Dispatch the same enter and submit events produced by the platform file-manager bridge.
fn drop_paths(fixture: &Fixture, paths: &[&str], cx: &mut TestAppContext) -> gpui_kit::Result<()> {
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        let position = gpui_kit::point(gpui_kit::px(100.), gpui_kit::px(100.));
        window.dispatch_event(
            FileDropEvent::Entered {
                position,
                paths: ExternalPaths(paths.iter().map(PathBuf::from).collect()),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        window.dispatch_event(FileDropEvent::Ended.to_platform_input(), cx);
        window.render_frame(cx);
    })
}

/// Entering and leaving shows transient feedback without opening anything; empty drops are inert.
#[gpui_kit::test]
fn hover_exit_and_empty_drop(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture(cx)?;
        cx.update_window(fixture.window, |_, window, cx| {
            window.render_frame(cx);
            assert!(!cx.has_active_drag());
            assert!(!window.find("file-drop-hint").visible());
            window.dispatch_event(
                FileDropEvent::Entered {
                    position: gpui_kit::point(gpui_kit::px(100.), gpui_kit::px(100.)),
                    paths: ExternalPaths([PathBuf::from("hover.parquet")].into_iter().collect()),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            assert!(cx.has_active_drag());
            assert!(window.find("file-drop-hint").visible());
            window.dispatch_event(FileDropEvent::Exited.to_platform_input(), cx);
            window.render_frame(cx);
            assert!(!cx.has_active_drag());
            assert!(!window.find("file-drop-hint").visible());
        })?;
        drop_paths(&fixture, &[], cx)?;
        cx.run_until_parked();
        assert!(
            fixture
                .backend
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Empty and populated layouts accept multiple files, preserve path text and reuse duplicate tabs.
#[gpui_kit::test]
async fn files_open_and_duplicates_reuse_tabs(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        drop_paths(&fixture, &["中文 file.parquet", "second.pq"], cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            fixture.view.read(cx).tabs.len() == 2 && !fixture.view.read(cx).busy
        })
        .await;
        cx.update_window(fixture.window, |_, window, cx| {
            assert_eq!(fixture.view.read(cx).active_tab, 1);
            assert!(!cx.has_active_drag());
            assert!(!window.find("file-drop-hint").visible());
        })?;
        drop_paths(&fixture, &["third.parquet", "中文 file.parquet"], cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            let view = fixture.view.read(cx);
            view.tabs.len() == 3 && view.active_tab == 0 && !view.busy
        })
        .await;
        assert_eq!(
            fixture
                .backend
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            4
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Backend failures use the existing error display without creating a tab or dropping earlier files.
#[gpui_kit::test]
async fn invalid_drop_preserves_existing_tabs(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        drop_paths(&fixture, &["good.parquet"], cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            fixture.view.read(cx).tabs.len() == 1 && !fixture.view.read(cx).busy
        })
        .await;
        drop_paths(&fixture, &["folder", "bad.parquet"], cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            fixture.view.read(cx).error
        })
        .await;
        cx.update_window(fixture.window, |_, _, cx| {
            let view = fixture.view.read(cx);
            assert_eq!(view.tabs.len(), 1);
            assert!(view.status.contains("folder"));
            assert!(view.status.contains("bad.parquet"));
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A newer submitted batch cancels the previous opener before its completion can replace the tabs.
#[gpui_kit::test]
async fn newer_drop_supersedes_pending_open(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        drop_paths(&fixture, &["old.parquet"], cx)?;
        let old = cx.update(|cx| fixture.view.read(cx).opening_cancel.clone());
        drop_paths(&fixture, &["new.parquet"], cx)?;
        assert!(matches!(old.check(), Err(QueryError::Cancelled)));
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            let view = fixture.view.read(cx);
            view.tabs.len() == 1 && !view.busy
        })
        .await;
        cx.update(|cx| assert_eq!(fixture.view.read(cx).tabs[0].source.name, "new.parquet"));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A failed file remains reported after a successful sibling's first query completes.
#[gpui_kit::test]
async fn mixed_drop_keeps_failure_notification(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let outcome = async {
        let fixture = fixture(cx)?;
        drop_paths(&fixture, &["bad.parquet", "good.parquet"], cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            let view = fixture.view.read(cx);
            view.tabs.len() == 1 && !view.busy
        })
        .await;
        cx.update_window(fixture.window, |_, window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
            assert!(!fixture.view.read(cx).error);
            assert_eq!(fixture.view.read(cx).tabs[0].source.name, "good.parquet");
            assert!(
                fixture
                    .view
                    .read(cx)
                    .table
                    .focus_handle(cx)
                    .is_focused(window)
            );
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}
