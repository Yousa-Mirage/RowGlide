//! Native GPUI presentation and interaction for an injected query backend.
//!
//! `run` is the public launch boundary. Workspace, table and cell implementation
//! modules stay private; concrete engine selection belongs to `rowglide-app`.

mod cells;
mod table;
mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use std::sync::Arc;

/// Initialize GPUI Kit and run the native window with the supplied backend.
///
/// Installs assets and the light theme, creates the workspace and quits when the
/// last window closes. The caller selects and constructs the backend; this layer
/// depends only on its shared interface. Invoke from the application's main
/// thread. Window creation failures are printed to stderr before quitting.
pub fn run(backend: Arc<dyn rowglide_core::Backend>) {
    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Light, None, cx);

        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(820.)),
                cx,
            ))),
            window_min_size: Some(size(px(920.), px(600.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Rowglide".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        if let Err(error) = open_window(options, cx, move |window, cx| {
            cx.new(|cx| workspace::Workspace::new(backend, window, cx))
        }) {
            eprintln!("Cannot open Rowglide: {error}");
            cx.quit();
        }

        cx.activate(true);
    });
}
