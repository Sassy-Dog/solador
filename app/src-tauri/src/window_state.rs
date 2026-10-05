//! Native window persistence, shared with the opt-in native smoke example.

pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri_plugin_window_state::StateFlags;

    builder.plugin(
        tauri_plugin_window_state::Builder::default()
            // Restore geometry and zoom, but always open visibly with the
            // configured decorations. A hidden window must not stay hidden.
            .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
            .with_filter(|label| label == "main")
            .build(),
    )
}
