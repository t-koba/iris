#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use iced::application;
use iced::window;
use iced::window::icon;
use iced::{Font, Settings, Size};
use iris_ui_shell::app;
#[cfg(not(target_os = "macos"))]
use mimalloc::MiMalloc;
use tracing::warn;

const APP_ID: &str = "iris";
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/iris-icon-256.png");

/// mimalloc everywhere except macOS, where the system allocator reports VM usage more steadily
/// in Activity Monitor for an image-heavy GUI.
#[cfg(not(target_os = "macos"))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Both fonts are subsets:
// - Noto Sans CJK JP: ASCII, Latin extensions, symbols, kana, the CJK Unified Ideographs block
//   (U+4E00-9FFF), compatibility ideographs and half/full-width forms; 16 MB -> 5.3 MB.
// - Material Symbols Outlined: only the code points used in the code (48 glyphs); 10 MB -> 83 KB.
const NOTO_SANS_CJK_JP_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSansCJKjp-Regular-subset.otf");
const MATERIAL_SYMBOLS_OUTLINED: &[u8] =
    include_bytes!("../../../assets/fonts/MaterialSymbolsOutlined-subset.ttf");

fn main() -> iced::Result {
    iris_ui_shell::observability::init_tracing();

    let builder = application(app::init, app::update, app::view)
        .settings(application_settings())
        .window(window_settings())
        .title(app::title)
        .theme(app::theme)
        .subscription(app::subscription)
        .font(NOTO_SANS_CJK_JP_REGULAR)
        .font(MATERIAL_SYMBOLS_OUTLINED)
        .default_font(Font::with_name("Noto Sans CJK JP"));

    builder.run()
}

fn application_settings() -> Settings {
    Settings {
        id: Some(APP_ID.to_string()),
        ..Settings::default()
    }
}

fn window_settings() -> window::Settings {
    let settings = window::Settings {
        icon: load_window_icon(),
        // Closing goes through `CloseRequested` so pending settings are written before exit.
        exit_on_close_request: false,
        min_size: Some(Size::new(
            iris_ui_shell::app::resolved_layout::MIN_WINDOW_WIDTH,
            iris_ui_shell::app::resolved_layout::MIN_WINDOW_HEIGHT,
        )),
        ..window::Settings::default()
    };

    #[cfg(target_os = "linux")]
    let settings = {
        let mut settings = settings;
        settings.platform_specific.application_id = APP_ID.to_string();
        settings
    };

    #[cfg(target_os = "macos")]
    let settings = {
        let mut settings = settings;
        settings.platform_specific.title_hidden = true;
        settings.platform_specific.titlebar_transparent = true;
        settings.platform_specific.fullsize_content_view = true;
        settings
    };

    settings
}

fn load_window_icon() -> Option<window::Icon> {
    let decoded = match image::load_from_memory(WINDOW_ICON_PNG) {
        Ok(decoded) => decoded.into_rgba8(),
        Err(error) => {
            warn!("failed to decode window icon: {error}");
            return None;
        }
    };

    let (width, height) = decoded.dimensions();

    match icon::from_rgba(decoded.into_raw(), width, height) {
        Ok(icon) => Some(icon),
        Err(error) => {
            warn!("failed to build window icon: {error}");
            None
        }
    }
}
