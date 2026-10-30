//! Native window persistence, shared with the opt-in native smoke example.

#[cfg(target_os = "macos")]
#[path = "window_state_macos.rs"]
mod macos;

#[cfg(target_os = "macos")]
pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(macos::plugin())
}

/// Restore geometry and zoom, but always open visibly with the configured
/// decorations. A hidden window must not stay hidden.
#[cfg(not(target_os = "macos"))]
fn state_flags() -> tauri_plugin_window_state::StateFlags {
    use tauri_plugin_window_state::StateFlags;
    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED
}

#[cfg(not(target_os = "macos"))]
pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(
        tauri_plugin_window_state::Builder::default()
            .with_state_flags(state_flags())
            .with_filter(|label| label == "main")
            .build(),
    )
}

/// Write the window's geometry to disk now, as the plugin's `RunEvent::Exit`
/// handler would on a normal quit.
///
/// For the one exit that is not a normal quit: the updater plugin calls
/// `std::process::exit(0)` on Windows once the installer is launched
/// (`update::before_exit`, #705), and that skips `RunEvent::Exit`. A failed save
/// is logged and not fatal; losing a window position must not stand between an
/// operator and the update they asked for.
#[cfg(windows)]
pub fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri_plugin_window_state::AppHandleExt as _;
    if let Err(e) = app.save_window_state(state_flags()) {
        eprintln!("update: could not save the window state before exiting: {e}");
    }
}
