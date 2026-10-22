//! Window geometry: the preferred opening size, the fit to the primary
//! display, and the client-area chrome the window asks for.

use gpui_kit::{App, Bounds, Pixels, WindowBounds, px, size};

/// Preferred first-window size. The origin is computed from the primary
/// display so the window opens in the middle of the usable area, and smaller
/// screens shrink the request rather than clipping it.
pub(super) const WINDOW_SIZE: gpui_kit::Size<Pixels> = size(px(1440.), px(900.));

/// Smallest edge the window accepts, and the floor a smaller screen may
/// shrink the default down to. It holds the fixed sidebar, one readable chat
/// column, and a docked Details panel; the user can drag the window smaller
/// than the opening size, down to this.
pub(super) const WINDOW_FLOOR: gpui_kit::Size<Pixels> = size(px(960.), px(600.));

/// Opening bounds plus the minimum size for the main window.
///
/// The default size is fitted to the primary display's usable area. A
/// persisted windowed size wins over the default and may be smaller — the
/// user's own resize is honored down to [`WINDOW_FLOOR`].
pub(super) fn startup_geometry(
    cx: &App,
    saved: Option<mycode_config::WindowSize>,
) -> (Bounds<Pixels>, gpui_kit::Size<Pixels>) {
    let saved = saved.map(|saved| size(px(saved.width as f32), px(saved.height as f32)));
    let Some(display) = cx.primary_display() else {
        let (window, min) = fit_window(None, saved);
        let origin = gpui_kit::point(px(80.), px(48.));
        return (
            Bounds {
                origin,
                size: window,
            },
            min,
        );
    };
    let screen = display.visible_bounds();
    let (window, min) = fit_window(Some(screen.size), saved);
    (Bounds::centered_at(screen.center(), window), min)
}

/// Window size and minimum size for a usable screen area. Pure so the sizing
/// rules are testable without a display.
pub(super) fn fit_window(
    screen: Option<gpui_kit::Size<Pixels>>,
    saved: Option<gpui_kit::Size<Pixels>>,
) -> (gpui_kit::Size<Pixels>, gpui_kit::Size<Pixels>) {
    let fit = |desired: gpui_kit::Size<Pixels>| match screen {
        Some(screen) => size(
            fit_edge(screen.width, desired.width, WINDOW_FLOOR.width),
            fit_edge(screen.height, desired.height, WINDOW_FLOOR.height),
        ),
        None => desired,
    };
    let min = fit(WINDOW_FLOOR);
    let window = saved.map_or_else(
        || fit(WINDOW_SIZE),
        |saved| {
            let fitted = fit(saved);
            size(fitted.width.max(min.width), fitted.height.max(min.height))
        },
    );
    (window, min)
}

fn fit_edge(available: Pixels, desired: Pixels, floor: Pixels) -> Pixels {
    let room = (available - px(64.)).max(floor.min(available));
    desired.min(room)
}

/// Position the native macOS controls inside the custom 40px title bar.
pub(super) const TRAFFIC_LIGHTS: gpui_kit::Point<Pixels> = gpui_kit::point(px(16.), px(14.));

/// Window options for the app-drawn chrome.
///
/// The system title bar stays hidden on every desktop:
/// - Windows: `appears_transparent` extends the client area over the caption.
///   Min, max, and close still go through `WindowControlArea`.
/// - macOS: `appears_transparent` and `app_owns_titlebar_drag` drop the system
///   title bar and its drag/double-click. Native traffic lights stay visible.
pub(super) fn client_window_options(
    bounds: Bounds<Pixels>,
    min_size: gpui_kit::Size<Pixels>,
) -> gpui_kit::WindowOptions {
    let mut options = gpui_kit::component::TitleBar::window_options();
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    options.window_min_size = Some(min_size);
    options.app_owns_titlebar_drag = true;
    let titlebar = options
        .titlebar
        .get_or_insert_with(gpui_kit::component::TitleBar::title_bar_options);
    titlebar.title = Some("MYCode".into());
    titlebar.appears_transparent = true;
    titlebar.traffic_light_position = Some(TRAFFIC_LIGHTS);
    options
}

/// Removes the 1px DWM border Windows 11 draws around borderless windows.
///
/// `appears_transparent` already drops the caption; without this call DWM
/// still strokes a system-colored edge. `DWMWA_COLOR_NONE` suppresses it.
/// Older Windows versions lack the attribute and simply return an error.
#[cfg(windows)]
pub(super) fn strip_dwm_border(window: &gpui_kit::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFF_FF_FF_FE;
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    unsafe {
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as _,
            &DWMWA_COLOR_NONE as *const u32 as *const core::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub(super) fn strip_dwm_border(_: &gpui_kit::Window) {}

