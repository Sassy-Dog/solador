//! The toolbar's "Fit window to tiles": resize the main window's height so the
//! webview shows exactly its content, keeping the width.
//!
//! An app-defined command rather than the JS window API on purpose: the
//! capability file grants the frontend no `core:window:*` permission, and
//! widening that ACL for one resize is not worth the argument. The fitted size
//! is persisted like any other resize, because the window-state module
//! captures every `Resized` event.
//!
//! The resize is relative: the window changes by the difference between the
//! content and the webview's own viewport, both measured by the page. Tauri's
//! title-bar accounting cannot be used for an absolute size. On macOS
//! `inner_size` and `outer_size` both report the frame, title bar included,
//! and `set_size` sets that frame. Asking for the content height as an
//! absolute size cut 28pt off the bottom of the tiles.

use tauri::{LogicalSize, WebviewWindow};

/// `tauri.conf.json`'s `minHeight` for the main window. The OS clamps to it
/// anyway; restating it keeps the requested size equal to the one we get, and
/// a test holds the two equal.
const MIN_HEIGHT: f64 = 400.0;

/// The window height, in logical points, that turns a `viewport`-tall webview
/// into a `content`-tall one, clamped to what the window can be. A `max`
/// below `min` (a monitor shorter than the window's floor) yields `min`: the
/// floor is the window's, so the monitor cannot lower it.
fn fitted_height(current: f64, content: f64, viewport: f64, min: f64, max: f64) -> f64 {
    (current + content.ceil() - viewport).min(max).max(min)
}

#[tauri::command]
pub fn window_fit_height(
    window: WebviewWindow,
    content_height: f64,
    viewport_height: f64,
) -> Result<(), String> {
    fit(&window, content_height, viewport_height)
        .map_err(|e| format!("Couldn't fit the window: {e}"))
}

fn fit(window: &WebviewWindow, content: f64, viewport: f64) -> Result<(), String> {
    for (name, value) in [("content", content), ("viewport", viewport)] {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!(
                "the measured {name} height {value} is not a height"
            ));
        }
    }
    if window.is_fullscreen().map_err(|e| e.to_string())? {
        return Err("exit full screen first".into());
    }
    // A maximized window has no height of its own to fit; the click asks for
    // one, so leave the maximized state rather than ignore the request.
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())?;
    }
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let inner = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale);
    let outer = window
        .outer_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale);
    // The tallest window the monitor can show is its work area (menu bar and
    // Dock excluded), less whatever the platform counts outside `inner`. No
    // monitor answer means no ceiling we can name.
    let max = match window.current_monitor().map_err(|e| e.to_string())? {
        Some(monitor) => {
            let area = monitor
                .work_area()
                .size
                .to_logical::<f64>(monitor.scale_factor());
            area.height - (outer.height - inner.height)
        }
        None => f64::INFINITY,
    };
    let height = fitted_height(inner.height, content, viewport, MIN_HEIGHT, max);
    window
        .set_size(LogicalSize::new(inner.width, height))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_moves_by_the_gap_between_content_and_viewport() {
        // macOS: a 911pt frame holds an 883pt webview showing 680pt of tiles.
        assert_eq!(fitted_height(911.0, 680.0, 883.0, 400.0, 960.0), 708.0);
        // Content taller than the viewport grows the window.
        assert_eq!(fitted_height(600.0, 772.0, 572.0, 400.0, 960.0), 800.0);
        // Content is rounded up, so the last pixel row is never cut.
        assert_eq!(fitted_height(800.0, 612.2, 772.0, 400.0, 960.0), 641.0);
    }

    #[test]
    fn the_result_stays_within_the_window_s_range() {
        assert_eq!(fitted_height(900.0, 100.0, 872.0, 400.0, 960.0), 400.0);
        assert_eq!(fitted_height(600.0, 2000.0, 572.0, 400.0, 960.0), 960.0);
        assert_eq!(
            fitted_height(600.0, 2000.0, 572.0, 400.0, f64::INFINITY),
            2028.0
        );
    }

    #[test]
    fn a_monitor_shorter_than_the_floor_cannot_lower_it() {
        assert_eq!(fitted_height(600.0, 900.0, 572.0, 400.0, 300.0), 400.0);
    }

    #[test]
    fn min_height_matches_the_window_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let window = &conf["app"]["windows"][0];
        assert_eq!(window["label"], "main");
        assert_eq!(window["minHeight"].as_f64(), Some(MIN_HEIGHT));
    }
}
