//! Fixtures for view tests: a headless window with gpui-kit, the theme and
//! [`Services`] set up as `main` does, but with the config in a temp dir.
//!
//! Tests are `#[gpui_kit::test]` functions taking `&mut TestAppContext`; they
//! render real views, click real elements by `ElementId` and read the view's
//! state back. See `gpui_kit::test` for the window API.

use gpui_kit::base::Root;
use gpui_kit::*;
use tethys_adapters::config_toml::TomlConfigStore;

use crate::theme;
use crate::workspace::Services;

/// A config store in a fresh temp dir, so a test starts from nothing saved.
pub fn temp_config(name: &str) -> TomlConfigStore {
    let dir = std::env::temp_dir().join(format!(
        "tethys-view-{name}-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    TomlConfigStore::new(dir)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Sets the app up as `main` does: gpui-kit, [`Services`] with `config`, the
/// theme and the tab layout.
pub fn init(config: TomlConfigStore, cx: &mut App) {
    gpui_kit::init(cx);
    let mut services = Services::new();
    services.config = config;
    cx.set_global(services);
    theme::init(cx);
    crate::tab_skin::init(cx);
}

/// Opens a headless window of `size` whose content is `build`'s view.
pub fn open_window<V: Render>(
    cx: &mut TestAppContext,
    size: Size<Pixels>,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> (WindowHandle<Root>, Entity<V>) {
    cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size,
        };
        let (window, content) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            build,
        )
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), content)
    })
}
