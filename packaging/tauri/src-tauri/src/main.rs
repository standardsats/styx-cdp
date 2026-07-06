//! The Tauri shell: start the bundled `styx-app` on a fixed loopback port, wait for it, and
//! point the webview at that origin. There is no second frontend - the UI is exactly what
//! the browser gets, so the desktop build cannot drift from the served app.
//!
//! Not part of the workspace build: this needs the platform webview toolchain and the Tauri
//! CLI. `cargo tauri build` on a machine with the GUI stack produces the bundles.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The child styx-app, killed when this guard drops - closing the window must not leave a
/// wallet server (with a hot funding key) running headless.
struct AppProcess(Child);

impl Drop for AppProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fixed loopback port for the desktop session. Fixed, not ephemeral: the app binds it
/// itself (via `STYX_APP_LISTEN`), and two desktop instances sharing a port is a clear
/// "already running" failure, not a silent second wallet.
const PORT: u16 = 9781;

/// The app config path: an env override, else the platform config dir.
fn config_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("STYX_APP_CONFIG") {
        return p.into();
    }
    dirs::config_dir().unwrap_or_else(|| ".".into()).join("styx").join("app.toml")
}

/// Spawn styx-app on the fixed loopback port. The bundled binary sits next to this one.
fn spawn_app() -> std::io::Result<AppProcess> {
    let exe = std::env::current_exe()?
        .parent()
        .map(|d| d.join("styx-app"))
        .unwrap_or_else(|| "styx-app".into());
    let child = Command::new(exe)
        .arg("--config")
        .arg(config_path())
        .env("STYX_APP_LISTEN", format!("127.0.0.1:{PORT}"))
        .spawn()?;
    Ok(AppProcess(child))
}

fn wait_ready(addr: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn main() {
    // `app` lives for the whole of main: `run` blocks until the window closes, then returns,
    // and this drops - killing styx-app. No headless wallet survives the window.
    let _app = spawn_app().expect("failed to launch styx-app");
    let base = format!("http://127.0.0.1:{PORT}");
    if !wait_ready(&format!("127.0.0.1:{PORT}"), Duration::from_secs(10)) {
        eprintln!("styx-app did not come up on {base}");
        return; // `_app` drops here, killing the child
    }
    tauri::Builder::default()
        .setup(move |handle| {
            let url = base.parse().expect("valid url");
            tauri::WebviewWindowBuilder::new(handle, "main", tauri::WebviewUrl::External(url))
                .title("STYX")
                .inner_size(1100.0, 820.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
