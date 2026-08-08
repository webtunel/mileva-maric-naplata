//! Tauri command surface used by the kiosk frontend (admin commands live in `admin.rs`).

use std::io::Write as _;
use std::sync::atomic::Ordering;

use tauri::{Manager, State};

use crate::models::{
    Cart, DeviceStatus, KioskError, KioskResult, PaymentOutcome, PrintedTicket, Settings,
};
use crate::payment::PaymentEnd;
use crate::AppState;

/// Current in-memory settings snapshot (prices, museum name, max tickets, device ids).
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> KioskResult<Settings> {
    Ok(state.settings.lock().clone())
}

/// None for empty/whitespace strings — treats a blank config field as "unset".
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

/// Append a timestamped line to app_data/debug.log — payment-flow diagnostics for the
/// deployed kiosk (no console there). Best effort; never fails the caller.
pub fn dlog(app: &tauri::AppHandle, msg: &str) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("debug.log"))
        {
            let ts = chrono::Utc::now().format("%H:%M:%S%.3f");
            let _ = writeln!(f, "[{ts}] {msg}");
        }
    }
}

/// Releases the `payment_active` reservation on drop, on every return path.
struct ActiveGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Prices the cart, then runs a real NV9 cash session and resolves once the visitor has
/// paid in full (or errors/cancels).
///
/// MONEY-SAFETY: a `pending` sale row is written BEFORE any cash is accepted, and the
/// running inserted total is persisted on every credit. So cash in the box is never only
/// in memory (survives power loss). On full payment the sale is finalized `paid` and its
/// tickets minted & stored (before any printing — a printer failure never loses the sale).
/// On cancel/error/timeout with partial cash the sale is marked `abandoned` with the amount
/// taken, so staff can reconcile/refund. Tickets are minted at completion, not submission.
#[tauri::command]
pub async fn start_payment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cart: Cart,
) -> KioskResult<PaymentOutcome> {
    // Atomic reservation: reject a second concurrent session fighting for the serial port.
    if state
        .payment_active
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(KioskError::Other("transakcija je već u toku".into()));
    }
    let _active = ActiveGuard(&state.payment_active);

    let settings = state.settings.lock().clone();

    // Price the cart with checked arithmetic; enforce the ticket cap server-side.
    let mut total: i64 = 0;
    let mut qty_total: u32 = 0;
    for line in &cart.lines {
        if line.qty == 0 {
            continue;
        }
        let ty = settings
            .ticket_types
            .iter()
            .find(|t| t.code == line.code)
            .ok_or_else(|| KioskError::Other(format!("nepoznat tip karte: {}", line.code)))?;
        qty_total = qty_total
            .checked_add(line.qty)
            .ok_or_else(|| KioskError::Other("previše karata".into()))?;
        let line_total = ty
            .price_rsd
            .checked_mul(i64::from(line.qty))
            .ok_or_else(|| KioskError::Other("iznos prekoračen".into()))?;
        total = total
            .checked_add(line_total)
            .ok_or_else(|| KioskError::Other("iznos prekoračen".into()))?;
    }
    if qty_total == 0 || total <= 0 {
        return Err(KioskError::Other("prazna korpa".into()));
    }
    if qty_total > settings.max_total_tickets {
        return Err(KioskError::Other(format!(
            "najviše {} karata po transakciji",
            settings.max_total_tickets
        )));
    }

    let sale_id = uuid::Uuid::new_v4().to_string();
    let session_started = chrono::Utc::now().timestamp();

    dlog(&app, &format!("start_payment: total={total} qty={qty_total} sale={sale_id}"));

    // 1) Money-safety anchor: a pending row exists before any cash is taken.
    state
        .db
        .create_pending_sale(&sale_id, session_started, total, qty_total)?;
    dlog(&app, "pending sale created");

    // 2) Run the session; persist the inserted total on every credit.
    let cfg = crate::config::nv9_cfg(&settings);
    let (tx, rx) = std::sync::mpsc::channel::<PaymentEnd>();
    let credit_db = state.db.clone();
    let credit_sale = sale_id.clone();
    let handle = crate::payment::start(
        app.clone(),
        cfg,
        total,
        move |inserted: i64| {
            let _ = credit_db.update_inserted(&credit_sale, inserted);
        },
        move |end| {
            let _ = tx.send(end);
        },
    );
    *state.payment.lock() = Some(handle);

    // 3) Wait off the webview thread so progress events + cancel keep flowing.
    dlog(&app, "waiting for session end");
    let end = tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|_| KioskError::Other("plaćanje prekinuto".into()))?
        .map_err(|_| KioskError::Other("plaćanje prekinuto".into()))?;
    dlog(
        &app,
        match &end {
            PaymentEnd::Paid(v) => format!("session end: Paid({v})"),
            PaymentEnd::Cancelled(v) => format!("session end: Cancelled({v})"),
            PaymentEnd::Failed(e, v) => format!("session end: Failed({e}, {v})"),
        }
        .as_str(),
    );

    // Take the handle out, then reap its threads in the BACKGROUND so the response — and
    // the frontend switch to the printing screen — never waits on validator/coordinator
    // thread cleanup (which can take a poll cycle or two on real hardware).
    let handle = state.payment.lock().take();
    tauri::async_runtime::spawn_blocking(move || drop(handle));

    let db = state.db.clone();
    match end {
        PaymentEnd::Paid(inserted) => {
            // Mint tickets at completion (correct issue time), then finalize atomically.
            let issued_at = chrono::Utc::now().timestamp();
            let tickets = mint_tickets(&settings, &cart, &state.secret, issued_at);
            dlog(&app, &format!("finalizing paid: {} tickets", tickets.len()));
            if let Err(e) = db.finalize_paid(&sale_id, inserted, &tickets) {
                // Money is in the box — never drop it. Journal the paid sale for recovery.
                dlog(&app, &format!("finalize FAILED: {e}"));
                write_recovery(&app, &sale_id, inserted, total, &tickets, &e);
                return Err(KioskError::Other(format!(
                    "naplaćeno {inserted} RSD, ali upis karata nije uspeo: {e}. Sačuvano u recovery — pozovite osoblje."
                )));
            }
            dlog(&app, "finalized OK — returning outcome");
            Ok(PaymentOutcome {
                sale_id,
                inserted_rsd: inserted,
                total_rsd: total,
            })
        }
        PaymentEnd::Cancelled(inserted) => {
            if inserted > 0 {
                db.mark_abandoned(&sale_id, inserted)?;
            } else {
                let _ = db.delete_sale(&sale_id);
            }
            Err(KioskError::Other("transakcija otkazana".into()))
        }
        PaymentEnd::Failed(err, inserted) => {
            if inserted > 0 {
                let _ = db.mark_abandoned(&sale_id, inserted);
            } else {
                let _ = db.delete_sale(&sale_id);
            }
            Err(err)
        }
    }
}

/// Signals cancellation of the in-flight session (non-blocking; the coordinator drains
/// in-flight credits and resolves `start_payment`). No-op if nothing is running.
#[tauri::command]
pub fn cancel_payment(state: State<'_, AppState>) -> KioskResult<()> {
    if let Some(handle) = state.payment.lock().as_ref() {
        handle.cancel();
    }
    Ok(())
}

/// Prints a paid sale's tickets and marks it printed. Safe to call again (reprint).
#[tauri::command]
pub async fn print_tickets(
    state: State<'_, AppState>,
    sale_id: String,
) -> KioskResult<Vec<PrintedTicket>> {
    let sale = state
        .db
        .get_sale(&sale_id)?
        .ok_or_else(|| KioskError::Other(format!("prodaja nije pronađena: {sale_id}")))?;
    if sale.status != "paid" {
        return Err(KioskError::Other(
            "karte se mogu štampati samo za plaćenu transakciju".into(),
        ));
    }

    let settings = state.settings.lock().clone();
    let target = crate::config::printer_target(&settings);
    let museum = settings.museum_name.clone();
    let printer_port = non_empty(settings.printer_port.clone());
    let windows_name = non_empty(settings.printer_windows_name.clone());
    let feed_before = settings.feed_before_cut_mm;
    let feed_after = settings.feed_after_cut_mm;
    let paper_width = settings.paper_width_mm;
    let tickets = sale.tickets.clone();

    // Printer IO off the webview thread. Priority: Windows spooler (native driver) >
    // serial virtual COM > raw USB.
    let tickets_for_print = tickets.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(name) = windows_name {
            crate::printer::print_tickets_windows(&name, &museum, &tickets_for_print, feed_before, feed_after, paper_width)
        } else if let Some(port) = printer_port {
            crate::printer::print_tickets_serial(&port, &museum, &tickets_for_print, feed_before, feed_after, paper_width)
        } else {
            crate::printer::print_tickets(&target, &museum, &tickets_for_print, feed_before, feed_after, paper_width)
        }
    })
    .await
    .map_err(|_| KioskError::Print("štampa prekinuta".into()))??;

    state.db.mark_printed(&sale_id)?;
    Ok(tickets)
}

/// Live NV9 + printer probe for the kiosk UI. Skips probing while a session owns the port.
#[tauri::command]
pub async fn device_status(state: State<'_, AppState>) -> KioskResult<DeviceStatus> {
    if state.payment.lock().is_some() {
        return Ok(DeviceStatus {
            validator_connected: true,
            validator_detail: "zauzeto (transakcija u toku)".into(),
            printer_connected: true,
            printer_detail: "zauzeto (transakcija u toku)".into(),
        });
    }

    let settings = state.settings.lock().clone();
    let port = crate::config::nv9_cfg(&settings).port;
    let target = crate::config::printer_target(&settings);
    let printer_port = non_empty(settings.printer_port.clone());
    let windows_name = non_empty(settings.printer_windows_name.clone());

    tauri::async_runtime::spawn_blocking(move || {
        let (validator_connected, validator_detail) = match crate::nv9::probe(&port) {
            Ok(detail) => (true, detail),
            Err(e) => (false, e.to_string()),
        };
        let printer_result = if let Some(name) = &windows_name {
            crate::printer::probe_windows(name)
        } else if let Some(p) = &printer_port {
            crate::printer::probe_serial(p)
        } else {
            crate::printer::probe(&target)
        };
        let (printer_connected, printer_detail) = match printer_result {
            Ok(detail) => (true, detail),
            Err(e) => (false, e.to_string()),
        };
        DeviceStatus {
            validator_connected,
            validator_detail,
            printer_connected,
            printer_detail,
        }
    })
    .await
    .map_err(|_| KioskError::Other("provera uređaja prekinuta".into()))
}

/// Mint one signed ticket per unit in the cart. `issued_at` is stamped at payment completion.
fn mint_tickets(
    settings: &Settings,
    cart: &Cart,
    secret: &[u8],
    issued_at: i64,
) -> Vec<PrintedTicket> {
    let mut tickets = Vec::new();
    for line in &cart.lines {
        if line.qty == 0 {
            continue;
        }
        let Some(ty) = settings.ticket_types.iter().find(|t| t.code == line.code) else {
            continue;
        };
        for _ in 0..line.qty {
            let id = uuid::Uuid::new_v4().to_string();
            let claims = crate::token::TicketClaims {
                id: id.clone(),
                type_code: ty.code.clone(),
                price_rsd: ty.price_rsd,
                issued_at,
            };
            let qr_token = crate::token::sign(secret, &claims);
            tickets.push(PrintedTicket {
                id,
                type_code: ty.code.clone(),
                label: ty.label.clone(),
                price_rsd: ty.price_rsd,
                issued_at,
                qr_token,
            });
        }
    }
    tickets
}

/// Append a paid-but-unpersisted sale to an append-only recovery journal so a finalize
/// failure after cash is taken never loses the sale.
fn write_recovery(
    app: &tauri::AppHandle,
    sale_id: &str,
    inserted: i64,
    total: i64,
    tickets: &[PrintedTicket],
    err: &KioskError,
) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let record = serde_json::json!({
        "ts": chrono::Utc::now().timestamp(),
        "sale_id": sale_id,
        "inserted_rsd": inserted,
        "total_rsd": total,
        "error": err.to_string(),
        "tickets": tickets,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("recovery.jsonl"))
    {
        let _ = writeln!(file, "{record}");
    }
}
