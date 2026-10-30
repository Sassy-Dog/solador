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
pub(crate) fn state_flags() -> tauri_plugin_window_state::StateFlags {
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
