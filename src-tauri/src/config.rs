//! App bootstrap: HMAC secret lifecycle, default settings/PIN on first run,
//! and small helpers that turn `Settings` into the hardware configs `nv9`/`printer` expect.

use crate::models::{KioskResult, Settings};

const DEFAULT_ADMIN_PIN: &str = "1234";

/// Ensures the HMAC secret, default settings row, and default admin PIN exist in the DB.
/// Returns the raw (deobfuscated) HMAC secret to keep in memory for the session — this is
/// what gets stored in `AppState.secret` and used to sign/verify ticket QR tokens.
pub fn bootstrap(db: &crate::db::Db) -> KioskResult<Vec<u8>> {
    let raw = match db.get_secret("qr_hmac")? {
        Some(stored) => crate::token::deobfuscate(&stored),
        None => {
            let raw = crate::token::new_secret();
            db.set_secret("qr_hmac", &crate::token::obfuscate(&raw))?;
            raw
        }
    };

    // First run: persist a concrete settings row so the admin panel has something to
    // update in place (load_settings() already falls back to Settings::default() when
    // nothing is stored, but we want that default written down, not just implied).
    if db.get_secret("__bootstrapped")?.is_none() {
        db.save_settings(&Settings::default())?;
        db.set_secret("__bootstrapped", b"1")?;
    }

    // Default admin PIN, changeable later via admin_change_pin.
    if db.get_secret("admin_pin")?.is_none() {
        db.set_secret("admin_pin", DEFAULT_ADMIN_PIN.as_bytes())?;
    }

    Ok(raw)
}

/// Resolves NV9 serial port config: explicit setting wins, else first auto-detected
/// port, else a "COM3" fallback so Windows setup never fails to construct a config.
pub fn nv9_cfg(settings: &Settings) -> crate::nv9::Nv9Config {
    let port = settings
        .nv9_port
        .clone()
        .or_else(|| crate::nv9::list_ports().into_iter().next())
        .unwrap_or_else(|| "COM3".to_string());
    crate::nv9::Nv9Config { port, baud: 9600 }
}

/// Resolves the ESC/POS printer USB target from settings (None fields = auto-detect
/// the first bulk-out USB device, handled inside printer.rs).
pub fn printer_target(settings: &Settings) -> crate::printer::PrinterTarget {
    crate::printer::PrinterTarget {
        vendor_id: settings.printer_vendor_id,
        product_id: settings.printer_product_id,
    }
}
