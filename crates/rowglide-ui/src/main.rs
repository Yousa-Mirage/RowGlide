mod app;
mod cells;
mod grid;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use std::sync::Arc;
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
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
            if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    app::Workspace::new(Arc::new(rowglide_backend::MockBackend), window, cx)
                })
            }) {
                eprintln!("Cannot open Rowglide: {error}");
                cx.quit();
            }
            cx.activate(true);
        });
}
