# Kiosk za naplatu ulaznica — Muzej Milene Marić

Design + worker contract. Date: 2026-08-07.

## Cilj
Tauri v2 kiosk (Windows, fullscreen lockdown) koji prodaje muzejske karte, naplaćuje
gotovinom preko **NV9 bank note validatora (SSP)**, štampa karte na **ESC/POS USB
printeru** sa **jedinstvenim, offline-verifikovljivim QR kodom (HMAC-SHA256)**, i loguje
sve u lokalni **SQLite**. Skriveni admin panel za cene, istoriju i status.

## Ključne odluke
- **Bez kusura.** NV9 escrow drži novčanicu; ako bi prihvatanje premašilo dugovani iznos →
  **reject (vrati novčanicu)**. Sesija se završava kad `inserted == total`.
- Novac = **samo novčanice** (NV9). Nema zasebnog coin acceptora. Cene su umnošci 100 RSD.
- QR = HMAC-potpisan token, verifikuje se na kapiji **offline** istim ključem.
- HMAC ključ: čuva se **obfuskovan u SQLite `settings`** (mašina fizički zaključana).
- Admin: **5× tap gornji-levi ugao → PIN** (default PIN u settings, menja se).
- Frontend: **vanilla TS + Vite**, fontovi (Playfair Display, Work Sans) + bg slika
  bundlovani **lokalno** (kiosk radi bez interneta).

## Kritični invarijanti (NIKAD prekršiti)
1. **Nikad ne uzmi pare bez karte.** Sale + tiketi se perzistiraju u DB **pre** slanja na
   štampu. Ako štampa padne → sale ostaje `printed=false`, admin/korisnik može reprint.
   Novac je već u kutiji; karta se duguje korisniku dok se ne odštampa.
2. **Escrow reject** kad bi novčanica premašila `total` (no-change).
3. QR token je jedinstven po karti (uuid) i **verifikovljiv offline**.
4. Frontend nikad ne "simulira" novac u produkciji — progres dolazi iz pravih NV9 evenata.

## Arhitektura / threading
- Tauri (tokio) glavni proces drži `AppState` (`parking_lot::Mutex`) sa: `Db`, `Settings`,
  handle ka payment sesiji, `DeviceStatus`.
- **NV9 drajver = zaseban OS thread** (blocking serial poll ~200ms). Komunicira sa
  orkestratorom preko `crossbeam-channel`: šalje `PaymentEvent`, prima `EscrowDecision`.
- **Payment orchestrator** prima evente, primenjuje escrow logiku, emituje
  `payment://progress` (Tauri event) ka frontendu, na kraju vraća `PaymentOutcome`.
- Feature flag `simulate` (dev na macOS): simulator emituje fake evente umesto pravog NV9.

## Deljeni tipovi
Svi u `src/models.rs` (već napisano — NE menjati potpise). Bitni: `TicketType`, `Cart`,
`PrintedTicket`, `SaleRecord`, `PaymentEvent`, `PaymentProgress`, `PaymentOutcome`,
`DeviceStatus`, `Settings`, `KioskError`/`KioskResult`.

---

## MODUL CONTRACTS (svaki worker puni JEDAN fajl, protiv OVIH potpisa)

### `nv9.rs` — SSP drajver  [Codex W1]
ITL Smiley Secure Protocol preko `serialport` (9600 8N1, NV9 USB = FTDI virtual COM).
- SSP paket: `STX(0x7F) | SEQ/slaveID | LENGTH | DATA... | CRCL | CRCH`.
  CRC16, poly `0x8005`, seed `0xFFFF`, računat preko SEQ..DATA (bez STX). STX 0x7F u
  podacima se **byte-stuff-uje** (0x7F 0x7F).
- SEQ bit toggluje po uspešnoj komandi. slave addr default `0x00`.
- Komande koje treba: Sync `0x11`, Host Protocol Version `0x06` (v6+), Setup Request `0x05`
  (parsiraj kanale→vrednosti valute), Set Channel Inhibits `0x02`, Enable `0x0A`,
  Disable `0x09`, Poll `0x07`, Reject `0x08`, Get Serial Number `0x0C`.
- Poll odgovori (event bajtovi) mapiraj: Read `0xEF`, Credit `0xEE`, Note Rejecting `0xED`,
  Note Rejected `0xEC`, Note Stacked `0xEB`, Stacked `0xCC`, Note Held in escrow → koristi
  Read+hold, Disabled `0xE8`, Note Clear (returned) `0xE7`, Unsafe jam `0xE6`, Stacker full `0xE7`...
  (koristi zvanične SSP kodove; escrow = Read event sa kanalom != 0 dok nije stacked).
- **Escrow mode:** posle Setup, drži note u escrow-u (ne auto-stack). Na Read(kanal) →
  emit `NoteInEscrow{value}`. Orchestrator odlučuje: ako accept → sledeći Poll ostavi da se
  stack-uje (šalje ništa / hold=0); ako reject → pošalji `Reject 0x08`. Na Note Stacked →
  emit `Credited`. Na Note Rejected/returned → `NoteReturned`.
- eSSP enkripcija: NIJE obavezna za MVP (radi u plain SSP-u). Ostavi TODO hook.

Javni API:
```rust
pub struct Nv9Config { pub port: String, pub baud: u32 }
pub enum EscrowDecision { Accept, Reject }
/// Otvara port, radi sync+setup+enable, pa u petlji poll-uje.
/// Emituje PaymentEvent kroz `events`, čita odluke kroz `decisions`.
/// Vraća kad `stop` postane true ili na fatalnu grešku.
pub fn run_validator(
    cfg: Nv9Config,
    events: crossbeam_channel::Sender<crate::models::PaymentEvent>,
    decisions: crossbeam_channel::Receiver<EscrowDecision>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::models::KioskResult<()>;
/// Brza provera dostupnosti za DeviceStatus (open+sync+get serial, pa zatvori).
pub fn probe(port: &str) -> crate::models::KioskResult<String>;
/// Lista kandidat portova (FTDI/USB serial).
pub fn list_ports() -> Vec<String>;
```
Uz to: `#[cfg(feature="simulate")] pub fn run_simulator(...)` sa istim potpisom kao
`run_validator` (emit Connected, pa na svaki "insert" tick emit escrow/credit da UI radi).

### `printer.rs` — ESC/POS USB  [Codex W2]
Raw ESC/POS preko `rusb` (USB bulk OUT), bez eksternog escpos crate-a.
- Auto-detekcija: prvi USB uređaj bulk-out interfejsa; ili zadati `vendor/product` iz Settings.
- Layout karte (58/80mm): centriran naziv muzeja (Playfair vibe = bold), tip karte, cena,
  datum/vreme, kod (#id skraćen), pa **QR** ispod. Rez, feed, **partial cut** (GS V 1).
- QR: prvo probaj **native ESC/POS QR** (`GS ( k` model 2, size ~6, EC M). Kao fallback:
  renderuj `qrcode` crate → monochrome raster → `GS v 0`.
- Diakritika (ć č ž š đ): postavi codepage **CP852** (`ESC t 18`) i mapiraj Latin-2; ako
  mapiranje nije pouzdano, transliteriši u ASCII za tekst (QR sadrži čist token pa je svejedno).

Javni API:
```rust
pub struct PrinterTarget { pub vendor_id: Option<u16>, pub product_id: Option<u16> }
pub struct TicketPrintData<'a> {
    pub museum_name: &'a str,
    pub ticket: &'a crate::models::PrintedTicket,
    pub index: u32,   // 1-based
    pub total: u32,
}
pub fn print_tickets(target: &PrinterTarget, museum: &str,
    tickets: &[crate::models::PrintedTicket]) -> crate::models::KioskResult<()>;
pub fn probe(target: &PrinterTarget) -> crate::models::KioskResult<String>; // za DeviceStatus
```

### `db.rs` — SQLite  [Codex W3]
`rusqlite` (bundled). DB u app data dir (`app_data_dir/kiosk.sqlite`). WAL mode.
Šeme: `settings(key TEXT PK, value TEXT)`, `sales(id, created_at, total_rsd, inserted_rsd,
num_tickets, printed INTEGER, reprinted_count)`, `tickets(id PK, sale_id FK, type_code,
label, price_rsd, issued_at, qr_token, printed_at, redeemed_at NULL)`.
Idempotentne migracije na `init`.

Javni API:
```rust
pub struct Db { /* Mutex<Connection> ili r2d2 nije potreban, single conn + Mutex */ }
pub fn open(path: &std::path::Path) -> crate::models::KioskResult<Db>;
impl Db {
  pub fn load_settings(&self) -> crate::models::KioskResult<crate::models::Settings>;
  pub fn save_settings(&self, s: &crate::models::Settings) -> crate::models::KioskResult<()>;
  pub fn get_secret(&self, name: &str) -> crate::models::KioskResult<Option<Vec<u8>>>;
  pub fn set_secret(&self, name: &str, val: &[u8]) -> crate::models::KioskResult<()>;
  pub fn record_sale(&self, sale: &crate::models::SaleRecord) -> crate::models::KioskResult<()>;
  pub fn mark_printed(&self, sale_id: &str) -> crate::models::KioskResult<()>;
  pub fn inc_reprint(&self, sale_id: &str) -> crate::models::KioskResult<()>;
  pub fn get_sale(&self, sale_id: &str) -> crate::models::KioskResult<Option<crate::models::SaleRecord>>;
  pub fn list_sales(&self, from_ts: i64, to_ts: i64) -> crate::models::KioskResult<Vec<crate::models::SaleRecord>>;
  pub fn redeem_ticket(&self, ticket_id: &str) -> crate::models::KioskResult<bool>; // za gate (dup detekcija)
  pub fn export_csv(&self, from_ts: i64, to_ts: i64) -> crate::models::KioskResult<String>;
}
pub struct ZReport { pub from_ts: i64, pub to_ts: i64, pub count_sales: u32,
    pub count_tickets: u32, pub total_rsd: i64, pub by_type: Vec<(String,u32,i64)> }
impl Db { pub fn z_report(&self, from_ts: i64, to_ts: i64) -> crate::models::KioskResult<ZReport>; }
```

### `token.rs` — QR HMAC  [Codex W4a]
Format (pipe-delimited, kompaktan): `MMM|v1|{id}|{type}|{price}|{ts}|{mac}` gde
`mac = base64url( HMAC_SHA256(secret, "MMM|v1|{id}|{type}|{price}|{ts}") )` (bez paddinga).
```rust
pub struct TicketClaims { pub id: String, pub type_code: String, pub price_rsd: i64, pub issued_at: i64 }
pub fn sign(secret: &[u8], c: &TicketClaims) -> String;
pub fn verify(secret: &[u8], token: &str) -> crate::models::KioskResult<TicketClaims>;
/// Generiše/učita 32B ključ; obfuskacija = XOR sa fiksnom app maskom pre upisa u DB.
pub fn obfuscate(raw: &[u8]) -> Vec<u8>;
pub fn deobfuscate(stored: &[u8]) -> Vec<u8>;
```

### `payment.rs` — orchestrator  [Codex W4b]
```rust
pub struct PaymentHandle { /* stop flag, join handle, decisions sender */ }
/// Startuje NV9 (ili simulator) thread, escrow petlju, emit `payment://progress`.
/// Na `inserted == total` emit final progress {complete:true} i vrati kroz `on_done`.
pub fn start(app: tauri::AppHandle, cfg: crate::nv9::Nv9Config, total_rsd: i64,
    on_done: impl FnOnce(crate::models::KioskResult<i64>) + Send + 'static
) -> PaymentHandle;
impl PaymentHandle { pub fn cancel(&self); }
```
Escrow pravilo: na `NoteInEscrow{value}` → ako `inserted + value > total` pošalji
`EscrowDecision::Reject`, inače `Accept`. Na `Credited` update inserted, emit progress.

### `config.rs` — app config + ključ  [Codex W7 zajedno sa commands]
Secret key lifecycle: na startu `get_secret("qr_hmac")`; ako None → generiši 32B (uuid/
os rng preko `getrandom` kroz uuid? koristi `rand`? — koristi `token.rs` helper +
`std` — deterministički iz uuid v4 x2), obfuskuj, `set_secret`. Vrati raw ključ u memoriju.
Default PIN "1234" u settings ako ne postoji (admin ga menja).

### `commands.rs` + `lib.rs` wiring  [Codex W7]
Tauri komande (svi vraćaju `Result<_, KioskError>`):
- `get_config() -> Settings`
- `start_payment(cart: Cart) -> PaymentOutcome`  (blokira dok ne plati/otkaže; emit progress usput)
- `cancel_payment()`
- `print_tickets(sale_id: String) -> Vec<PrintedTicket>` (štampa + mark_printed; reprint safe)
- `finalize_and_print(cart: Cart, sale_id: String)` — ili spoji u start_payment koji vrati sale sa tiketima
- `device_status() -> DeviceStatus`
- Admin: `admin_login(pin) -> bool`, `admin_get_settings`, `admin_set_prices(types)`,
  `admin_list_sales(from,to) -> Vec<SaleRecord>`, `admin_zreport(from,to) -> ZReport`,
  `admin_reprint(sale_id)`, `admin_change_pin(old,new)`, `admin_export_csv(from,to)->String`.
`lib.rs`: build Tauri, single-instance plugin, autostart plugin, manage `AppState`,
generiraj tiket tokene pri prodaji (uuid + token.sign), kiosk lockdown (config već fullscreen).
`main.rs`: `fn main(){ kiosk_naplata_lib::run() }`.

### Frontend `src/`  [Codex W5]
Port DC designa (4 ekrana) na vanilla TS. Fajlovi: `index.html`, `main.ts`, `state.ts`,
`screens/{welcome,select,pay,success}.ts`, `admin.ts`, `styles.css`, lokalni fontovi u
`fonts/`, `assets/mileva-maric.jpg` (već tu).
- Ekrani i stil 1:1 sa DC designom (boje `#152225`/`#6B8E7F`/`#F4F1EA`, Playfair+Work Sans,
  bg slika sa animacijom, ista kompozicija). Rezolucija 1920×1080.
- `pay` ekran: `listen('payment://progress')` → update UBAČENO / progress bar iz PRAVIH evenata.
  Nema fake "Ubaci novac" dugmeta (osim kad je build `simulate`).
- `select`: +/- po tipu, maks ukupno iz configa, total.
- `success`: prikaz odštampanih karata (iz outcome), dugme reprint ako treba, "Nova transakcija".
- Admin: nevidljiva zona 120×120px gore-levo; 5 tapova <2s → PIN modal → panel
  (cene edit, lista prodaja + Z-report za dan, reprint, status uređaja, promena PIN, export CSV).
- Tauri API preko `@tauri-apps/api/core` (`invoke`) i `.../event` (`listen`).

### Admin backend  [Codex W6]
`admin.rs` — komande gore + PIN verifikacija (constant-time compare), Z-report agregacija
poziva `db.z_report`. Reprint poziva `db.get_sale` + `printer.print_tickets` + `db.inc_reprint`.

---

## Review (Claude)
- **W8 arhitektura/threading:** deadlock/panic u channelima, AppHandle Send/Sync, thread cleanup
  na cancel, single-instance, lockdown potpunost.
- **W9 edge/security:** invariant #1 (pare bez karte), escrow race, note-can't-make-exact
  deadlock (UX: "nema odgovarajućih apoena, pozovite osoblje" + cancel/refund-manual), power
  loss mid-tx, QR replay/dup (redeem_ticket), PIN brute-force, HMAC ključ lifecycle, CP852.

## Verifikacija (main)
`cargo check` (macOS, bez `simulate` i sa `simulate`), `npm run build` (tsc+vite).
Windows .exe build = `npm run tauri build` na Windows mašini (dokumentovati).
