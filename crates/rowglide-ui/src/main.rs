//! Native application startup and backend injection.

mod cells;
mod table;
mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use std::sync::Arc;

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
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

        if let Err(error) = open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                workspace::Workspace::new(
                    Arc::new(rowglide_core::DuckDbBackend::default()),
                    window,
                    cx,
                )
            })
        }) {
            eprintln!("Cannot open Rowglide: {error}");
            cx.quit();
        }

        cx.activate(true);
    });
}
