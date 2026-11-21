//! Application commands shared by the native macOS menu and the in-window menu bar.

use crate::i18n::tr;
use gpui_kit::*;

/// Exit the entire application, including all open file tabs.
#[derive(Clone, PartialEq, Debug, Default, Action)]
#[action(namespace = rowglide, no_json)]
struct Quit;

/// Register the localized menus before any window creates its menu bar.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    let menus = [
        Menu::new(tr!("menu.file")).items([MenuItem::action(tr!("menu.quit"), Quit)]),
        Menu::new(tr!("menu.help")),
    ];

    if cfg!(target_os = "macos") {
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        // AppKit uses the first submenu as the application menu; keep File in its own slot.
        cx.set_menus(std::iter::once(Menu::new("Rowglide")).chain(menus));
    } else {
        base::GlobalState::global_mut(cx)
            .set_app_menus(menus.into_iter().map(Menu::owned).collect());
    }
}
