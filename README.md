# Kiosk za naplatu ulaznica — Muzej Milene Marić

Samostalni kiosk (Tauri v2, Windows) koji prodaje muzejske karte za gotovinu,
naplaćuje preko **NV9 bank note validatora (SSP)**, štampa ulaznice sa
**jedinstvenim, offline-verifikovljivim QR kodom** na **ESC/POS USB printeru**, i
loguje svaku prodaju u lokalni **SQLite**. Skriveni admin panel za cene, izveštaje,
status uređaja i izlaz iz programa.

Model naplate: **bez kusura.** Transakcija se zatvori tek kad uneseni iznos == cena.
NV9 escrow drži svaku novčanicu; ako bi prihvatanje premašilo dugovani iznos, novčanica
se **vraća** posetiocu. (Cene su umnošci 100 RSD radi uplativosti novčanicama.)

## Arhitektura

```
frontend (vanilla TS + Vite)            src-tauri (Rust)
  index.html                              lib.rs        app setup, AppState, plugini, komande
  src/main.ts, state.ts, api.ts           commands.rs   get_config/start_payment/print_tickets/…
  src/screens/{welcome,select,pay,        payment.rs    orkestrator: NV9 event → escrow → progress
               success}.ts                nv9.rs        SSP drajver (CRC16, escrow, poll) + simulator
  src/admin.ts, styles.css                printer.rs    ESC/POS USB (raw), QR (GS(k + raster fallback)
                                          db.rs         SQLite (settings/secrets/sales/tickets), Z-report
                                          token.rs      HMAC-SHA256 QR token + obfuskacija ključa
                                          admin.rs      admin komande (PIN, cene, izveštaji, exit)
                                          config.rs     bootstrap ključa/podešavanja, NV9/printer cfg
```

Ekran `pay` se pokreće PRAVIM NV9 event-ima preko Tauri event-a `payment://progress`.

## Preduslovi (Windows build mašina)

- **Rust** (stable) + MSVC build tools
- **Node.js** ≥ 18
- **WebView2 Runtime** (na Win11 već postoji)
- **libusb/WinUSB drajver za printer**: ESC/POS USB printer mora imati WinUSB drajver da
  bi ga `rusb` otvorio — instalirati preko [Zadig](https://zadig.akeo.ie/) (izaberi printer
  interfejs → WinUSB). Ako koristiš proizvođački drajver + Windows red za štampu umesto ovoga,
  vidi „Alternativa: GDI štampa" dole.
- **NV9**: USB varijanta se predstavlja kao FTDI virtuelni COM port (npr. `COM3`).

## Build

```bash
npm install
npm run tauri build      # → src-tauri/target/release/bundle/nsis/*.exe (instaler)
```

Ikone su već generisane (`src-tauri/icons/`). Za nove: `npx tauri icon <slika-1024x1024.png>`.

## Dev / test bez hardvera (simulator)

NV9 drajver ima simulator iza `simulate` feature-a — emituje lažne uplate da UI radi bez
uređaja:

```bash
npm run tauri dev -- --features simulate
```

Bez feature-a, dev koristi pravi NV9 na konfigurisanom COM portu.

## Konfiguracija hardvera

Podešavanja su u SQLite (`%APPDATA%/rs.muzejmilenemaric.kiosk/kiosk.sqlite`), menjaju se
kroz admin panel ili direktno:

- **NV9 port** (`nv9_port`): `null` = auto-detekcija prvog USB serijskog porta; ili npr. `"COM3"`.
- **Printer** (`printer_vendor_id`/`printer_product_id`): `null` = prvi ESC/POS/bulk-OUT USB
  uređaj; ili zadati USB VID/PID (heksadecimalno u kodu, decimalno u bazi).

Status oba uređaja vidi se u admin panelu → tab **Uređaji**.

## Admin panel (skriveno)

- Otvaranje: **5 brzih tapova na nevidljivu zonu gore-levo** (120×120 px) unutar 2 s → PIN.
- **Podrazumevani PIN: `1234` — OBAVEZNO promeniti** (tab PIN).
- Tabovi: **Cene** (samo umnošci 100 RSD), **Prodaje / Z-izveštaj** (danas, + reprint po prodaji),
  **Uređaji** (status NV9 + printer), **PIN**, **Izvoz** (CSV za period).
- **Izađi iz programa**: dugme u zaglavlju panela (dupli klik za potvrdu) — traži PIN ponovo,
  pa gasi aplikaciju (za održavanje; prozor je inače zaključan: fullscreen, bez zatvaranja).

## Kiosk lockdown

`tauri.conf.json` postavlja: fullscreen, bez dekoracija, always-on-top, `closable:false`,
skip taskbar, single-instance, autostart na login. Za potpuni kiosk režim koristiti i Windows
**Assigned Access** ili group policy da se zaključa OS okruženje.

## Sigurnost novca (invarijanta)

**Nikad se ne uzima novac bez traga.** Životni ciklus prodaje:
1. `pending` red se upiše u bazu **pre** nego što se prihvati ijedna novčanica.
2. Na **svaku** primljenu novčanicu ažurira se `inserted_rsd` (novac nikad nije samo u
   memoriji — preživljava nestanak struje).
3. Puna uplata → `paid`, karte se kuju i upisuju **pre** štampe. Štampa je zasebna komanda,
   pa pad štampača ne gubi prodaju (`printed=false`, reprintuj iz admin panela).
4. Otkazivanje / greška / neaktivnost sa delimičnim novcem → `abandoned` sa iznosom koji je
   uzet (za osoblje: uskladiti/vratiti). Bez izdatih karata.

Dodatne zaštite:
- **Cancel-drain:** na otkazivanje sistem još 3 s hvata novčanicu koja je već krenula u
  kasu, da se i ona evidentira.
- **Neaktivnost 120 s:** posetilac ode sa delimično ubačenim novcem → sesija se zatvori i
  upiše kao `abandoned`; sledeći posetilac ne nasleđuje njegov novac.
- **Recovery žurnal:** ako upis karata padne posle naplate, prodaja + karte se dopisuju u
  `%APPDATA%/rs.muzejmilenemaric.kiosk/recovery.jsonl` (nikad se ne gubi).
- Admin → **Prodaje**: kolona Status pokazuje `Plaćeno` / `Plaćeno · nije štampano` /
  `Napušteno`; Z-izveštaj razdvaja naplaćeno od napuštenog novca.

## Preporučeno pojačanje (nije implementirano — odluka na tebi)

Kiosk je fizički zaključan i webview je CSP-om vezan za `self` (bez daljinskih skripti),
pa je realan rizik nizak, ali za dubinsku odbranu razmotri:
- **Admin session-token**: sada admin komande veruju frontendu da je PIN provereno; dodati
  server-side token koji `admin_login` izdaje, a svaka `admin_*` komanda zahteva.
- **PIN hashovanje + zaključavanje**: PIN se čuva kao tekst u `secrets`; dodati salt-hash i
  brojač neuspelih pokušaja sa backoff-om.
- **eSSP enkripcija** NV9 linka (sada plain SSP; hook označen u `nv9.rs`).

## QR token — offline verifikacija na ulazu

Svaka karta nosi QR sa tokenom:

```
MMM|v1|{ticketId}|{typeCode}|{priceRsd}|{issuedAtUnix}|{macBase64Url}
mac = base64url_nopad( HMAC_SHA256(secret, "MMM|v1|{id}|{type}|{price}|{ts}") )
```

Aplikacija na ulaznoj kapiji verifikuje token **offline** istim HMAC ključem
(`token::verify`) i sprečava dupli ulazak preko `db.redeem_ticket` (atomsko označavanje).
HMAC ključ se generiše na prvom pokretanju i čuva obfuskovan (XOR) u SQLite `secrets` tabeli.

## Podaci

- Baza: `%APPDATA%/rs.muzejmilenemaric.kiosk/kiosk.sqlite` (WAL).
- Tabele: `settings`, `secrets`, `sales`, `tickets`. Z-izveštaj i CSV izvoz iz admin panela.

## Napomene / granice

- **Bez kusura**: ako posetilac ima samo novčanicu veću od preostalog iznosa, ona se odbija;
  posetilac dopuni odgovarajućim apoenom ili otkaže. Admin može proširiti prihvaćene apoene.
- **eSSP enkripcija** NV9-a nije uključena (radi u plain SSP-u); hook je označen u `nv9.rs`.
- **Alternativa: GDI štampa** — ako ne želiš WinUSB/`rusb`, `printer.rs` se može zameniti
  Windows spooler štampom; QR se tada renderuje kao raster (fallback put već postoji).
