//! Administratorske komande za podešavanja, izveštaje i proveru uređaja.

use crate::models::{
    DeviceStatus, KioskError, KioskResult, PrintedTicket, SaleRecord, Settings, TicketType,
};
use crate::AppState;

const DEFAULT_PIN: &str = "1234";

#[tauri::command]
pub async fn admin_login(
    state: tauri::State<'_, AppState>,
    pin: String,
) -> Result<bool, KioskError> {
    let stored_pin = get_pin(&state.db)?;
    if ct_eq(&pin, &stored_pin) {
        return Ok(true);
    }

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    Ok(false)
}

#[tauri::command]
pub async fn admin_get_settings(state: tauri::State<'_, AppState>) -> Result<Settings, KioskError> {
    Ok(state.settings.lock().clone())
}

#[tauri::command]
pub async fn admin_set_prices(
    state: tauri::State<'_, AppState>,
    ticket_types: Vec<TicketType>,
) -> Result<(), KioskError> {
    if ticket_types.is_empty() {
        return Err(KioskError::Config("lista tipova karata je prazna".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for ticket_type in &ticket_types {
        let code = ticket_type.code.trim();
        // `code` is embedded in the pipe-delimited QR token — a '|' or empty code would
        // mint tickets that can never verify at the gate.
        if code.is_empty() || ticket_type.code.contains('|') {
            return Err(KioskError::Config(format!(
                "neispravan kod tipa karte: '{}'",
                ticket_type.code
            )));
        }
        if !seen.insert(code.to_owned()) {
            return Err(KioskError::Config(format!("dupli kod tipa karte: '{code}'")));
        }
        if ticket_type.price_rsd <= 0 || ticket_type.price_rsd % 100 != 0 {
            return Err(KioskError::Config(format!(
                "cena za '{}' mora biti pozitivan umnožak 100 ({})",
                ticket_type.code, ticket_type.price_rsd
            )));
        }
    }

    let mut settings = state.settings.lock();
    settings.ticket_types = ticket_types;
    state.db.save_settings(&*settings)?;
    drop(settings);
    Ok(())
}

#[tauri::command]
pub async fn admin_list_sales(
    state: tauri::State<'_, AppState>,
    from_ts: i64,
    to_ts: i64,
) -> Result<Vec<SaleRecord>, KioskError> {
    state.db.list_sales(from_ts, to_ts)
}

#[tauri::command]
pub async fn admin_zreport(
    state: tauri::State<'_, AppState>,
    from_ts: i64,
    to_ts: i64,
) -> Result<crate::db::ZReport, KioskError> {
    state.db.z_report(from_ts, to_ts)
}

#[tauri::command]
pub async fn admin_reprint(
    state: tauri::State<'_, AppState>,
    sale_id: String,
) -> Result<(), KioskError> {
    let sale = state
        .db
        .get_sale(&sale_id)?
        .ok_or_else(|| KioskError::Other(format!("prodaja '{}' nije pronađena", sale_id)))?;
    let settings = state.settings.lock().clone();
    print_via_settings(&settings, &settings.museum_name, &sale.tickets)?;
    state.db.inc_reprint(&sale_id)?;
    Ok(())
}

/// Prints a single fake ticket so staff can test the thermal printer from admin without a
/// real sale. The QR carries a real signed test token, so layout/QR/cut/feed all match live.
#[tauri::command]
pub async fn admin_test_print(state: tauri::State<'_, AppState>) -> Result<(), KioskError> {
    let settings = state.settings.lock().clone();
    let now = chrono::Utc::now().timestamp();
    let id = uuid::Uuid::new_v4().to_string();
    let claims = crate::token::TicketClaims {
        id: id.clone(),
        type_code: "test".into(),
        price_rsd: 0,
        issued_at: now,
    };
    let ticket = PrintedTicket {
        qr_token: crate::token::sign(&state.secret, &claims),
        id,
        type_code: "test".into(),
        label: "TEST KARTA".into(),
        price_rsd: 0,
        issued_at: now,
    };
    print_via_settings(&settings, &settings.museum_name, &[ticket])
}

/// Selects the print transport (Windows spooler > serial COM > raw USB) from settings and
/// prints the given tickets with the configured feed/width. Shared by reprint + test print.
fn print_via_settings(
    settings: &Settings,
    museum: &str,
    tickets: &[PrintedTicket],
) -> KioskResult<()> {
    let before = settings.feed_before_cut_mm;
    let after = settings.feed_after_cut_mm;
    let width = settings.paper_width_mm;
    let win = settings
        .printer_windows_name
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let port = settings
        .printer_port
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    if let Some(name) = win {
        crate::printer::print_tickets_windows(name, museum, tickets, before, after, width)
    } else if let Some(p) = port {
        crate::printer::print_tickets_serial(p, museum, tickets, before, after, width)
    } else {
        let target = crate::printer::PrinterTarget {
            vendor_id: settings.printer_vendor_id,
            product_id: settings.printer_product_id,
        };
        crate::printer::print_tickets(&target, museum, tickets, before, after, width)
    }
}

#[tauri::command]
pub async fn admin_change_pin(
    state: tauri::State<'_, AppState>,
    old_pin: String,
    new_pin: String,
) -> Result<(), KioskError> {
    let stored_pin = get_pin(&state.db)?;
    if !ct_eq(&old_pin, &stored_pin) {
        return Err(KioskError::BadPin);
    }
    if new_pin.len() < 4 || !new_pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(KioskError::Config("PIN mora imati bar 4 cifre".into()));
    }

    set_pin(&state.db, &new_pin)?;
    Ok(())
}

#[tauri::command]
pub async fn admin_export_csv(
    state: tauri::State<'_, AppState>,
    from_ts: i64,
    to_ts: i64,
) -> Result<String, KioskError> {
    state.db.export_csv(from_ts, to_ts)
}

#[tauri::command]
pub async fn admin_device_status(
    state: tauri::State<'_, AppState>,
) -> Result<DeviceStatus, KioskError> {
    let (nv9_port, vendor_id, product_id) = {
        let settings = state.settings.lock();
        (
            settings.nv9_port.clone(),
            settings.printer_vendor_id,
            settings.printer_product_id,
        )
    };

    let validator_port = nv9_port.or_else(|| crate::nv9::list_ports().into_iter().next());
    let (validator_connected, validator_detail) = match validator_port {
        Some(port) => match crate::nv9::probe(&port) {
            Ok(detail) => (true, detail),
            Err(error) => (false, error.to_string()),
        },
        None => (false, "nijedan port nije pronađen".into()),
    };

    let target = crate::printer::PrinterTarget {
        vendor_id,
        product_id,
    };
    let (printer_connected, printer_detail) = match crate::printer::probe(&target) {
        Ok(detail) => (true, detail),
        Err(error) => (false, error.to_string()),
    };

    Ok(DeviceStatus {
        validator_connected,
        validator_detail,
        printer_connected,
        printer_detail,
    })
}

/// Postavlja portove uređaja (NV9 COM i printer virtuelni COM). Prazno = auto/USB.
#[tauri::command]
pub async fn admin_set_devices(
    state: tauri::State<'_, AppState>,
    nv9_port: Option<String>,
    printer_port: Option<String>,
    printer_windows_name: Option<String>,
    feed_before_mm: u32,
    feed_after_mm: u32,
    paper_width_mm: u32,
) -> Result<(), KioskError> {
    let mut settings = state.settings.lock();
    settings.nv9_port = normalize_port(nv9_port);
    settings.printer_port = normalize_port(printer_port);
    settings.printer_windows_name = normalize_port(printer_windows_name);
    settings.feed_before_cut_mm = feed_before_mm.min(200);
    settings.feed_after_cut_mm = feed_after_mm.min(200);
    settings.paper_width_mm = if paper_width_mm == 0 { 80 } else { paper_width_mm.min(120) };
    state.db.save_settings(&settings)?;
    Ok(())
}

/// Uključuje/isključuje jednostavni režim (bez touch-a, uvek jedna karta za odrasle).
#[tauri::command]
pub async fn admin_set_simple_mode(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), KioskError> {
    let mut settings = state.settings.lock();
    settings.simple_mode = enabled;
    state.db.save_settings(&settings)?;
    Ok(())
}

fn normalize_port(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Verifikuje PIN pa gasi aplikaciju — izlaz iz kiosk režima za održavanje.
#[tauri::command]
pub async fn admin_exit(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    pin: String,
) -> Result<(), KioskError> {
    let stored_pin = get_pin(&state.db)?;
    if !ct_eq(&pin, &stored_pin) {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        return Err(KioskError::BadPin);
    }
    app.exit(0);
    Ok(())
}

fn get_pin(db: &crate::db::Db) -> KioskResult<String> {
    match db.get_secret("admin_pin")? {
        Some(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        None => Ok(DEFAULT_PIN.to_string()),
    }
}

fn set_pin(db: &crate::db::Db, pin: &str) -> KioskResult<()> {
    db.set_secret("admin_pin", pin.as_bytes())
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let len_diff = (ab.len() ^ bb.len()) as u8;
    let n = ab.len().max(bb.len());
    let mut acc: u8 = len_diff;
    for i in 0..n {
        let x = *ab.get(i).unwrap_or(&0);
        let y = *bb.get(i).unwrap_or(&0);
        acc |= x ^ y;
    }
    acc == 0
}
