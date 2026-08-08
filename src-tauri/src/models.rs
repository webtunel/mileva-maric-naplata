//! Shared domain types — the contract every module compiles against.
//! Money is always stored as whole RSD (dinars). No floats anywhere.

use serde::{Deserialize, Serialize};

/// A purchasable ticket category. `code` is the stable machine key
/// (e.g. "odrasli"), `label` the Serbian display name, `price_rsd` whole dinars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketType {
    pub code: String,
    pub label: String,
    pub price_rsd: i64,
}

/// One line of the cart: how many of a given ticket type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartLine {
    pub code: String,
    pub qty: u32,
}

/// Cart submitted from the frontend when the visitor presses OK on the select screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cart {
    pub lines: Vec<CartLine>,
}

impl Cart {
    /// Total number of individual tickets.
    #[allow(dead_code)]
    pub fn total_qty(&self) -> u32 {
        self.lines.iter().map(|l| l.qty).sum()
    }
}

/// A ticket that has been sold and (about to be) printed.
/// `qr_token` is the offline-verifiable HMAC string encoded into the QR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintedTicket {
    pub id: String,        // uuid v4
    pub type_code: String, // TicketType.code
    pub label: String,     // display label at time of sale
    pub price_rsd: i64,    // price at time of sale (denormalized for accounting)
    pub issued_at: i64,    // unix seconds
    pub qr_token: String,  // MMM|v1|id|type|price|ts|hmac
}

/// A sale record. Lifecycle: `pending` (session started, money may be accumulating) →
/// `paid` (inserted == total, tickets minted) or `abandoned` (cancel/error/timeout with
/// partial cash in the box — recorded so staff can reconcile/refund; no tickets issued).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleRecord {
    pub id: String,           // uuid v4
    pub created_at: i64,      // unix seconds
    pub total_rsd: i64,       // amount owed
    pub inserted_rsd: i64,    // amount actually taken so far (== total_rsd when paid)
    pub num_tickets: u32,
    pub tickets: Vec<PrintedTicket>,
    pub reprinted_count: u32, // how many times the tickets were re-printed
    pub status: String,       // "pending" | "paid" | "abandoned"
    pub printed: bool,        // whether the tickets have been printed at least once
}

/// Events emitted by the NV9 driver thread (and the simulator) toward the
/// payment orchestrator. `total_inserted_rsd` is the running session total.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaymentEvent {
    /// Validator reachable and enabled.
    Connected,
    /// Validator link lost.
    Disconnected,
    /// A note is held in escrow. The orchestrator decides accept vs reject
    /// (reject when accepting would exceed the amount owed — no-change model).
    NoteInEscrow { value_rsd: i64 },
    /// A note was stacked; `value_rsd` credited. `total_inserted_rsd` is the new session total.
    Credited { value_rsd: i64, total_inserted_rsd: i64 },
    /// A note was returned to the visitor (rejected / would overpay).
    NoteReturned { value_rsd: i64 },
    /// Non-fatal validator message (jam cleared, cashbox out, etc.).
    Notice { message: String },
    /// Fatal error for this session.
    Error { message: String },
}

/// Progress payload pushed to the frontend on the `payment://progress` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentProgress {
    pub inserted_rsd: i64,
    pub total_rsd: i64,
    pub complete: bool,
    pub note: Option<String>,
}

/// Result of a payment session returned to the frontend once fully paid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentOutcome {
    pub sale_id: String,
    pub inserted_rsd: i64,
    pub total_rsd: i64,
}

/// Live status of attached hardware, for the admin panel.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceStatus {
    pub validator_connected: bool,
    pub validator_detail: String,
    pub printer_connected: bool,
    pub printer_detail: String,
}

/// Persisted, admin-editable configuration.
/// `#[serde(default)]` makes loading tolerant of older stored settings that predate a
/// newly added field — missing fields fall back to `Settings::default()` instead of
/// failing deserialization (which would crash startup).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ticket_types: Vec<TicketType>,
    pub max_total_tickets: u32,
    pub museum_name: String,
    /// NV9 serial port (e.g. "COM3"); None = auto-detect / simulate.
    pub nv9_port: Option<String>,
    /// Printer USB ids; None = auto-detect first ESC/POS device.
    pub printer_vendor_id: Option<u16>,
    pub printer_product_id: Option<u16>,
    /// Printer serial (virtual COM) port, e.g. "COM4" (Bixolon BXLVCOM4USB).
    pub printer_port: Option<String>,
    /// Windows printer name for raw-spooler printing, e.g. "BIXOLON SRP-Q300".
    /// Highest priority when set: uses the native Windows driver (most reliable).
    pub printer_windows_name: Option<String>,
    /// Simple mode: no touch. Always sells one adult ticket; when the exact
    /// price is inserted, the ticket prints automatically and the machine loops.
    pub simple_mode: bool,
    /// Blank paper (mm) fed BEFORE the cut, so the blade clears the ticket content.
    pub feed_before_cut_mm: u32,
    /// Blank paper (mm) fed AFTER the cut (no cut) — the tail that sticks out for the
    /// next ticket / how much more paper is pushed out when the ticket ejects.
    pub feed_after_cut_mm: u32,
    /// Paper width (mm), e.g. 58 or 80. Sets the ESC/POS print area so centering is
    /// correct for the loaded roll.
    pub paper_width_mm: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ticket_types: vec![
                TicketType { code: "odrasli".into(), label: "Odrasli".into(), price_rsd: 400 },
                TicketType { code: "deca".into(), label: "Deca (do 12)".into(), price_rsd: 200 },
                TicketType { code: "studenti".into(), label: "Studenti".into(), price_rsd: 300 },
            ],
            max_total_tickets: 10,
            museum_name: "Muzej Mileve Marić".into(),
            nv9_port: Some("COM3".into()),
            printer_vendor_id: None,
            printer_product_id: None,
            printer_port: None,
            printer_windows_name: Some("BIXOLON SRP-Q300".into()),
            simple_mode: false,
            feed_before_cut_mm: 48,
            feed_after_cut_mm: 50,
            paper_width_mm: 80,
        }
    }
}

/// Central error type shared across modules.
#[derive(Debug, thiserror::Error)]
pub enum KioskError {
    #[error("hardver: {0}")]
    Hardware(String),
    #[error("baza: {0}")]
    Db(String),
    #[error("token: {0}")]
    Token(String),
    #[error("stampa: {0}")]
    Print(String),
    #[error("konfiguracija: {0}")]
    Config(String),
    #[error("neispravan PIN")]
    BadPin,
    #[error("{0}")]
    Other(String),
}

impl serde::Serialize for KioskError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type KioskResult<T> = Result<T, KioskError>;
