//! Opt-in native smoke harness. Uses the production window-state registration
//! with a blank webview: no cockpit polling, credentials or user settings.
//! Run using the recipe in app/README.md; not part of headless `cargo test`.

#[path = "../src/window_state.rs"]
mod window_state;

use std::time::Duration;
use tauri::Manager;

fn main() {
    let mode = std::env::args()
        .nth(1)
        .expect("write, read, maximize or minimize");
    assert!(matches!(
        mode.as_str(),
        "write" | "read" | "maximize" | "minimize"
    ));
    let scratch = std::path::PathBuf::from(
        std::env::var_os("SOLADOR_WINDOW_SMOKE_DIR").expect("scratch directory"),
    );
    assert!(scratch.is_absolute());
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "app.solador.window-state-smoke".into();
    context.config_mut().app.windows[0].url =
        tauri::WebviewUrl::External("about:blank".parse().unwrap());
    let result_path = scratch.join(format!("{mode}.json"));
    window_state::configure(tauri::Builder::default())
        .setup(move |app| {
            // HOME/APPDATA must point inside scratch, protecting real state.
            let config_dir = app.path().app_config_dir()?;
            assert!(
                config_dir.starts_with(&scratch),
                "isolated config required: {config_dir:?}"
            );
            let window = app.get_webview_window("main").expect("main window");
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                if mode == "write" {
                    window
                        .set_size(tauri::LogicalSize::new(700.0, 500.0))
                        .unwrap();
                    window
                        .set_position(tauri::PhysicalPosition::new(120, 150))
                        .unwrap();
                    std::thread::sleep(Duration::from_millis(500));
                }
                if mode == "maximize" {
                    window.maximize().unwrap();
                    std::thread::sleep(Duration::from_millis(500));
                } else if mode == "minimize" {
                    window.minimize().unwrap();
                    std::thread::sleep(Duration::from_millis(500));
                }
                let size = window.inner_size().unwrap();
                let position = window.outer_position().unwrap();
                let logical = size.to_logical::<f64>(window.scale_factor().unwrap());
                if mode == "write" {
                    assert!((logical.width - 700.0).abs() < 1.0);
                    assert!((logical.height - 500.0).abs() < 1.0);
                }
                std::fs::write(
                    result_path,
                    serde_json::to_vec(&serde_json::json!({
                        "width": size.width, "height": size.height,
                        "x": position.x, "y": position.y,
                        "visible": window.is_visible().unwrap(),
                        "maximized": window.is_maximized().unwrap(),
                        "minimized": window.is_minimized().unwrap(),
                    }))
                    .unwrap(),
                )
                .unwrap();
                window.close().unwrap();
            });
            Ok(())
        })
        .run(context)
        .expect("native smoke app");
}
