//! Kiosk za naplatu ulaznica — Tauri app entry point and shared state.

mod admin;
mod commands;
mod config;
mod db;
mod models;
mod nv9;
mod payment;
mod printer;
mod token;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tauri::Manager;

/// Shared app state, managed by Tauri and injected into every command via `State<'_, AppState>`.
pub struct AppState {
    pub db: Arc<db::Db>,
    pub settings: parking_lot::Mutex<models::Settings>,
    pub secret: Vec<u8>,
    pub payment: parking_lot::Mutex<Option<payment::PaymentHandle>>,
    /// Atomic reservation so two rapid `start_payment` calls can't run concurrent sessions
    /// fighting over the one serial port.
    pub payment_active: AtomicBool,
}

pub fn run() {
    let builder = tauri::Builder::default();

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.show();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    builder
        .setup(|app| {
            #[cfg(desktop)]
            {
                use tauri_plugin_autostart::ManagerExt;
                let _ = app.autolaunch().enable();
            }

            // A kiosk autostarts with no console. If init fails (corrupt DB, unwritable
            // data dir), do NOT exit cleanly — log to a file and exit(1) so a restart
            // watchdog / Windows service recovery can act on the failure.
            if let Err(e) = init_state(app.handle()) {
                let msg = format!("greška pri inicijalizaciji kioska: {e}");
                let log = std::env::temp_dir().join("kiosk-startup-error.log");
                let _ = std::fs::write(&log, &msg);
                eprintln!("{msg}");
                std::process::exit(1);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::start_payment,
            commands::cancel_payment,
            commands::print_tickets,
            commands::device_status,
            admin::admin_login,
            admin::admin_get_settings,
            admin::admin_set_prices,
            admin::admin_list_sales,
            admin::admin_zreport,
            admin::admin_reprint,
            admin::admin_change_pin,
            admin::admin_export_csv,
            admin::admin_device_status,
            admin::admin_set_devices,
            admin::admin_set_simple_mode,
            admin::admin_test_print,
            admin::admin_exit,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| eprintln!("greška pri pokretanju aplikacije: {e}"));
}

/// Open the database, bootstrap the HMAC secret + defaults, and register shared state.
fn init_state(app: &tauri::AppHandle) -> models::KioskResult<()> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| models::KioskError::Config(e.to_string()))?;
    std::fs::create_dir_all(&app_dir).map_err(|e| models::KioskError::Config(e.to_string()))?;

    let db = db::open(&app_dir.join("kiosk.sqlite"))?;
    let secret = config::bootstrap(&db)?;
    let settings = db.load_settings()?;

    app.manage(AppState {
        db: Arc::new(db),
        settings: parking_lot::Mutex::new(settings),
        secret,
        payment: parking_lot::Mutex::new(None),
        payment_active: AtomicBool::new(false),
    });
    Ok(())
}
