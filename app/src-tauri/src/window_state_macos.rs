//! AppKit's desktop is measured in points. Restoring physical pixels before
//! the window acquires its destination screen scales them using the startup
//! screen instead, halving/doubling bounds on every launch on mixed-DPI Macs.
//! Keep normal bounds in points, including while minimized/maximized/fullscreen.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{plugin::TauriPlugin, LogicalPosition, LogicalSize, Manager, Runtime, Window};

const FILENAME: &str = ".window-state.json";

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CoordinateSpace {
    Logical,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Bounds {
    // Required discriminator: legacy plugin files contain pixels with no
    // saved scale. Guessing that scale can keep compounding corrupted bounds.
    coordinate_space: CoordinateSpace,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    maximized: bool,
}

#[derive(Deserialize, Serialize)]
struct SavedState {
    main: Bounds,
}

impl Bounds {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
    }

    fn read<R: Runtime>(window: &Window<R>) -> tauri::Result<Self> {
        let scale = window.scale_factor()?;
        let position = window.outer_position()?.to_logical::<f64>(scale);
        let size = window.inner_size()?.to_logical::<f64>(scale);
        Ok(Self {
            coordinate_space: CoordinateSpace::Logical,
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: window.is_maximized()?,
        })
    }

    fn restore<R: Runtime>(&self, window: &Window<R>) -> tauri::Result<()> {
        window.set_size(LogicalSize::new(self.width, self.height))?;
        // Compare everything in AppKit's shared point coordinate space.
        // Rectangle intersection also handles a window spanning a display.
        let on_screen = window.available_monitors()?.iter().any(|monitor| {
            let scale = monitor.scale_factor();
            let origin = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            self.x < origin.x + size.width
                && self.x + self.width > origin.x
                && self.y < origin.y + size.height
                && self.y + self.height > origin.y
        });
        if on_screen {
            window.set_position(LogicalPosition::new(self.x, self.y))?;
        }
        if self.maximized {
            window.maximize()?;
        }
        Ok(())
    }
}

type Cache = Arc<Mutex<Option<Bounds>>>;

fn capture<R: Runtime>(window: &Window<R>, cache: &Cache) {
    // Read the native window before locking: runtime calls can dispatch window
    // events, so holding a cache lock across them risks a reentrant deadlock.
    if window.is_minimized().unwrap_or(true) || window.is_fullscreen().unwrap_or(true) {
        return;
    }
    let Ok(current) = Bounds::read(window) else {
        return;
    };
    if !current.valid() {
        return;
    }
    let mut saved = cache.lock().unwrap();
    if current.maximized {
        if let Some(normal) = saved.as_mut() {
            normal.maximized = true;
        }
    } else {
        *saved = Some(current);
    }
}

fn save<R: Runtime>(app: &tauri::AppHandle<R>, cache: &Cache) {
    let Some(main) = *cache.lock().unwrap() else {
        return;
    };
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let directory = app.path().app_config_dir()?;
        std::fs::create_dir_all(&directory)?;
        // An interrupted write must not discard the previous usable geometry.
        let temporary = directory.join(".window-state.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&SavedState { main })?)?;
        std::fs::rename(temporary, directory.join(FILENAME))?;
        Ok(())
    };
    if let Err(error) = write() {
        eprintln!("Could not save window position: {error}");
    }
}

pub(super) fn plugin<R: Runtime>() -> TauriPlugin<R> {
    let cache: Cache = Arc::default();
    let events = Arc::clone(&cache);
    tauri::plugin::Builder::new("solador-window-state")
        .on_window_ready(move |window| {
            if window.label() != "main" {
                return;
            }
            let restored = window
                .app_handle()
                .path()
                .app_config_dir()
                .ok()
                .and_then(|dir| std::fs::read(dir.join(FILENAME)).ok())
                .and_then(|bytes| serde_json::from_slice::<SavedState>(&bytes).ok())
                .map(|saved| saved.main)
                .filter(Bounds::valid);
            *cache.lock().unwrap() = restored.or_else(|| Bounds::read(&window).ok());
            if let Some(bounds) = restored {
                if let Err(error) = bounds.restore(&window) {
                    eprintln!("Could not restore window position: {error}");
                }
            }
            let events = Arc::clone(&cache);
            let tracked = window.clone();
            window.on_window_event(move |event| match event {
                tauri::WindowEvent::Moved(_)
                | tauri::WindowEvent::Resized(_)
                | tauri::WindowEvent::ScaleFactorChanged { .. } => capture(&tracked, &events),
                tauri::WindowEvent::CloseRequested { .. } => {
                    capture(&tracked, &events);
                    save(tracked.app_handle(), &events);
                }
                _ => {}
            });
        })
        .on_event(move |app, event| match event {
            tauri::RunEvent::ExitRequested { .. } => {
                if let Some(window) = app.get_webview_window("main") {
                    capture(&window.as_ref().window(), &events);
                }
                save(app, &events);
            }
            tauri::RunEvent::Exit => save(app, &events),
            _ => {}
        })
        .build()
}
