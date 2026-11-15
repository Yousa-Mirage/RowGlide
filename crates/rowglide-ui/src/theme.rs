//! System-driven light/dark appearance for the Rowglide window.

use gpui_kit::component::Theme;
use gpui_kit::{App, Subscription, Window};

/// Apply the current system appearance of `window` to GPUI Kit's global theme.
///
/// Uses the window's appearance rather than the application-wide snapshot because
/// GPUI recommends the window value on Linux. Loading the matching light or dark
/// theme also refreshes the Base scrollbar and resize-handle projection and every
/// open window; no separate view notification is required.
///
/// GPUI Kit 0.9.1's default dark table header is too dim on the table background.
/// Dark mode therefore uses the muted surface plus the normal foreground for the
/// header; light mode keeps the upstream palette.
pub(crate) fn sync_system_theme(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
    if Theme::global(cx).is_dark() {
        Theme::update(cx, |theme| {
            theme.table_head = theme.muted;
            theme.table_head_foreground = theme.foreground;
        });
    }
}

/// Observe future system appearance changes for `window`.
///
/// The caller must retain the returned subscription for as long as the window's
/// view should follow the system. The callback reapplies the global theme and does
/// not retain the window or view; dropping the subscription stops future updates.
pub(crate) fn observe_system_theme(window: &mut Window) -> Subscription {
    window.observe_window_appearance(|window, cx| {
        sync_system_theme(window, cx);
    })
}

#[cfg(test)]
mod tests {
    use gpui_kit::WindowAppearance;
    use gpui_kit::component::ThemeMode;

    /// Vibrant macOS appearances must follow their base light or dark mode.
    #[test]
    fn vibrant_appearances_map_to_base_modes() {
        assert_eq!(ThemeMode::from(WindowAppearance::Light), ThemeMode::Light);
        assert_eq!(
            ThemeMode::from(WindowAppearance::VibrantLight),
            ThemeMode::Light
        );
        assert_eq!(ThemeMode::from(WindowAppearance::Dark), ThemeMode::Dark);
        assert_eq!(
            ThemeMode::from(WindowAppearance::VibrantDark),
            ThemeMode::Dark
        );
    }
}
