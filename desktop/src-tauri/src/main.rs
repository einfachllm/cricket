// Hides the console window on release builds on Windows (the actual target
// platform for this app); irrelevant on other OSes.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use tauri::Manager;

struct BackendUrl(String);

#[tauri::command]
fn get_backend_url(state: tauri::State<BackendUrl>) -> String {
  state.0.clone()
}

fn main() {
  tauri::Builder::default()
    .setup(|app| {
      let data_dir = app
        .path()
        .app_data_dir()
        .expect("could not resolve app data directory");
      std::fs::create_dir_all(&data_dir).expect("could not create app data directory");

      // Probe for the first free port so a dev server holding :8081 does not
      // block the app. The probe is dropped immediately; worst case the port
      // is raced before `run` binds and we fall back to log-and-open.
      let port = (8081..=8090).find(|port| {
        std::net::TcpListener::bind(format!("127.0.0.1:{port}"))
          .map(|probe| {
            drop(probe);
            true
          })
          .unwrap_or(false)
      });
      let Some(port) = port else {
        eprintln!("Harnesswurm backend failed to start: no free port in 8081..=8090");
        return Ok(());
      };

      app.manage(BackendUrl(format!("http://127.0.0.1:{port}")));

      // Embeds the proxy server directly in the desktop app's own process,
      // so launching the app is enough — no separate `cargo run` for the
      // backend. A bind failure is logged, not fatal: the window still opens
      // either way.
      tauri::async_runtime::spawn(async move {
        let config = harnesswurm_backend::ServerConfig {
          bind_addr: format!("127.0.0.1:{port}"),
          data_dir,
        };
        if let Err(err) = harnesswurm_backend::run(config).await {
          eprintln!("Harnesswurm backend failed to start: {err}");
        }
      });

      Ok(())
    })
    .invoke_handler(tauri::generate_handler![get_backend_url])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
