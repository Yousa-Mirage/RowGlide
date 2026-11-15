//! Headless freshness lifecycle coverage without OS timers or real file scans.

use super::super::Workspace;
use gpui_kit::test::TestAppContextExt;
use gpui_kit::{AnyWindowHandle, AppContext, Entity, TestAppContext, WindowOptions};
use rowglide_core::{
    Backend, BatchCursor, CacheLimits, Cancellation, MockBackend, Query, QueryError, ResultStore,
    Source,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

/// Counts queries and simulates reload failure without blocking the deterministic executor.
#[derive(Default)]
struct ReloadBackend {
    /// True makes reload fail while preserving the fixture registration.
    fail: AtomicBool,
    /// Simulates an external change observed by the periodic metadata task.
    changed: AtomicBool,
    /// Number of requests actually sent to the backend.
    queries: AtomicUsize,
}

impl Backend for ReloadBackend {
    /// Expose the standard mock sources, including nested data and an empty table.
    fn sources(&self) -> Vec<Source> {
        MockBackend.sources()
    }

    /// Return the controlled freshness state to exercise the real periodic task.
    fn source_changed(&self, _source: usize) -> Result<bool, QueryError> {
        Ok(self.changed.load(Ordering::Relaxed))
    }

    /// Return a fixture source after observing cancellation or the requested failure.
    fn reload(&self, source: usize, cancel: Cancellation) -> Result<Source, QueryError> {
        cancel.check()?;
        if self.fail.load(Ordering::Relaxed) {
            return Err(QueryError::Invalid("Reload fixture failed".into()));
        }
        self.sources()
            .into_iter()
            .find(|file| file.id == source)
            .ok_or_else(|| QueryError::Invalid("Unknown fixture".into()))
    }

    /// Count dispatches to verify changed tabs never start another query.
    fn query(
        &self,
        query: Query,
        cancel: Cancellation,
    ) -> Result<Box<dyn BatchCursor>, QueryError> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        MockBackend.query(query, cancel)
    }
}

/// Own the window, live workspace and observable backend for a freshness scenario.
struct Fixture {
    /// Window used to dispatch production handlers.
    window: AnyWindowHandle,
    /// Workspace whose tabs and retained results are inspected.
    view: Entity<Workspace>,
    /// Simulated file reload behavior.
    backend: Arc<ReloadBackend>,
}

/// Initialize the production workspace and its regular initial Browse query.
fn fixture(cx: &mut TestAppContext) -> Result<Fixture, Box<dyn std::error::Error>> {
    let results = ResultStore::new(CacheLimits::default())?;
    let backend = Arc::new(ReloadBackend::default());
    let (window, view) = cx.update(|cx| {
        gpui_kit::init(cx);
        super::super::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(backend.clone(), results, Vec::new(), None, window, cx))
        })
    })?;
    Ok(Fixture {
        window,
        view,
        backend,
    })
}

/// A changed active tab retains rows, blocks new queries and stays stale after failed reload.
/// Successful reload clears the warning, preserves SQL drafts and refreshes Browse normally.
#[gpui_kit::test]
async fn changed_tab_requires_successful_reload(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            !fixture.view.read(cx).busy
        })
        .await;
        let before = fixture.backend.queries.load(Ordering::Relaxed);
        cx.update_window(fixture.window, |_, window, cx| {
            fixture.view.update(cx, |view, cx| {
                let source = view.active_tab().source.id;
                view.sql
                    .update(cx, |editor, cx| editor.set_value("SELECT 42", window, cx));
                assert!(view.table.read(cx).delegate().has_result());
                view.mark_changed(&[source, usize::MAX], cx);
                assert!(view.active_tab().changed);
                assert!(!view.tabs[1].changed);
                view.run(window, cx);
                assert!(!view.busy);
                assert!(view.table.read(cx).delegate().has_result());
                fixture.backend.fail.store(true, Ordering::Relaxed);
                view.reload_source(window, cx);
            });
        })?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            fixture.view.read(cx).reloading.is_none()
        })
        .await;
        assert_eq!(fixture.backend.queries.load(Ordering::Relaxed), before);
        cx.update_window(fixture.window, |_, window, cx| {
            fixture.view.update(cx, |view, cx| {
                assert!(view.active_tab().changed);
                assert!(view.table.read(cx).delegate().has_result());
                fixture.backend.fail.store(false, Ordering::Relaxed);
                view.reload_source(window, cx);
            });
        })?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            let view = fixture.view.read(cx);
            view.reloading.is_none() && !view.busy
        })
        .await;
        cx.update(|cx| {
            let view = fixture.view.read(cx);
            assert!(!view.active_tab().changed);
            assert_eq!(view.active_tab().sql, "SELECT 42");
            assert!(view.table.read(cx).delegate().has_result());
        });
        assert_eq!(fixture.backend.queries.load(Ordering::Relaxed), before + 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A reload completion cannot recreate a closed tab, and neighboring warnings remain independent.
#[gpui_kit::test]
async fn closing_reloading_tab_ignores_completion(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            !fixture.view.read(cx).busy
        })
        .await;
        cx.update_window(fixture.window, |_, window, cx| {
            fixture.view.update(cx, |view, cx| {
                let source = view.active_tab().source.id;
                let neighbor = view.tabs[1].source.id;
                view.mark_changed(&[source, neighbor], cx);
                view.reload_source(window, cx);
                view.close_tab(source, window, cx);
                assert!(view.active_tab().changed);
                assert!(view.reloading.is_none());
            });
        })?;
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(fixture.view.read(cx).tabs.len(), 2));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A periodic sample marks inactive tabs too, without dispatching another data query.
#[gpui_kit::test]
async fn periodic_check_marks_all_sources(cx: &mut TestAppContext) {
    let outcome = async {
        let fixture = fixture(cx)?;
        cx.wait_for(fixture.window, Duration::from_secs(1), |_, cx| {
            !fixture.view.read(cx).busy
        })
        .await;
        let queries = fixture.backend.queries.load(Ordering::Relaxed);
        fixture.backend.changed.store(true, Ordering::Relaxed);
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        cx.update(|cx| assert!(fixture.view.read(cx).tabs.iter().all(|tab| tab.changed)));
        assert_eq!(fixture.backend.queries.load(Ordering::Relaxed), queries);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}
