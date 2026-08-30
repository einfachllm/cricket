#![cfg_attr(not(debug_assertions), target_os = "linux", features = ["tray-icon"])]

fn main() {
  tauri::Builder::default()
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
