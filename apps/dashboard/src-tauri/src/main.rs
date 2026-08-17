// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The embedded server runs missions in this process, so on Windows the
    // AppContainer launcher re-enters the desktop executable just as the CLI
    // launcher re-enters `kranz.exe`. Route the trusted private argv before
    // Tauri initializes a runtime or window.
    #[cfg(windows)]
    if kranz_engine::sandbox_windows::internal_launcher_requested() {
        match kranz_engine::sandbox_windows::run_internal_launcher() {
            Ok(code) => std::process::exit(code.clamp(0, 255) as i32),
            Err(error) => {
                eprintln!("kranz AppContainer launcher: {error}");
                std::process::exit(1);
            }
        }
    }

    app_lib::run();
}
