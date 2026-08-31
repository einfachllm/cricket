// Hides the console window on release builds on Windows (the actual target
// platform for this app); irrelevant on other OSes.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use tauri::Manager;

fn main() {
  tauri::Builder::default()
    .setup(|app| {
      let data_dir = app
        .path()
        .app_data_dir()
        .expect("could not resolve app data directory");
      std::fs::create_dir_all(&data_dir).expect("could not create app data directory");

      // Embeds the proxy server directly in the desktop app's own process,
      // so launching the app is enough — no separate `cargo run` for the
      // backend. A bind failure (e.g. a dev instance already holding the
      // port) is logged, not fatal: the window still opens either way.
      tauri::async_runtime::spawn(async move {
        let config = harnesswurm_backend::ServerConfig {
          bind_addr: "127.0.0.1:8081".to_string(),
          data_dir,
        };
        if let Err(err) = harnesswurm_backend::run(config).await {
          eprintln!("Harnesswurm backend failed to start: {err}");
        }
      });

      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
