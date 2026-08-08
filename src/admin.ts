// Hidden admin surface: 5 taps on the invisible top-left hotzone within 2s
// opens a PIN modal; a correct PIN opens the admin panel (prices, sales +
// Z-report, reprint, device status, PIN change, CSV export).

import {
  adminChangePin,
  adminExit,
  adminExportCsv,
  adminGetSettings,
  adminListSales,
  adminLogin,
  adminReprint,
  adminSetDevices,
  adminSetPrices,
  adminSetSimpleMode,
  adminTestPrint,
  adminZReport,
  deviceStatus,
  type DeviceStatus,
  type SaleRecord,
  type Settings,
  type TicketType,
  type ZReport,
} from "./api";
import { escapeHtml } from "./state";
import { getVersion } from "@tauri-apps/api/app";

const TAP_WINDOW_MS = 2000;
const TAPS_REQUIRED = 5;

type Tab = "prices" | "sales" | "devices" | "pin" | "export";

export function initAdmin(hotzone: HTMLElement, root: HTMLElement): void {
  let taps: number[] = [];
  hotzone.addEventListener("click", () => {
    const now = Date.now();
    taps.push(now);
    taps = taps.filter((t) => now - t <= TAP_WINDOW_MS);
    if (taps.length >= TAPS_REQUIRED) {
      taps = [];
      openPinModal(root);
    }
  });
}

function closeAdmin(root: HTMLElement): void {
  root.innerHTML = "";
}

function openPinModal(root: HTMLElement): void {
  root.innerHTML = `
    <div class="modal-backdrop" id="admin-backdrop">
      <div class="modal pin-modal">
        <h2>Admin pristup</h2>
        <input type="password" id="pin-input" class="pin-input" inputmode="numeric" maxlength="12" placeholder="PIN" />
        <div class="modal-error" id="pin-error"></div>
        <div class="modal-actions">
          <button type="button" class="btn btn-ghost" id="pin-cancel">Otkaži</button>
          <button type="button" class="btn btn-primary" id="pin-submit">Potvrdi</button>
        </div>
      </div>
    </div>
  `;

  const backdrop = root.querySelector<HTMLElement>("#admin-backdrop")!;
  const input = root.querySelector<HTMLInputElement>("#pin-input")!;
  const errorEl = root.querySelector<HTMLElement>("#pin-error")!;
  const cancelBtn = root.querySelector<HTMLButtonElement>("#pin-cancel")!;
  const submitBtn = root.querySelector<HTMLButtonElement>("#pin-submit")!;

  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) closeAdmin(root);
  });
  cancelBtn.addEventListener("click", () => closeAdmin(root));

  const submit = async (): Promise<void> => {
    const pin = input.value.trim();
    if (!pin) return;
    submitBtn.disabled = true;
    errorEl.textContent = "";
    try {
      const ok = await adminLogin(pin);
      if (ok) {
        openAdminPanel(root, pin);
      } else {
        errorEl.textContent = "Pogrešan PIN.";
        input.value = "";
        input.focus();
      }
    } catch (err) {
      errorEl.textContent = `Greška: ${String(err)}`;
    } finally {
      submitBtn.disabled = false;
    }
  };
  submitBtn.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") void submit();
  });
  input.focus();
}

function todayRange(): { from: number; to: number } {
  const now = new Date();
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 0, 0, 0, 0);
  return { from: Math.floor(start.getTime() / 1000), to: Math.floor(Date.now() / 1000) };
}

function openAdminPanel(root: HTMLElement, pin: string): void {
  root.innerHTML = `
    <div class="modal-backdrop" id="admin-backdrop">
      <div class="modal admin-panel">
        <div class="admin-panel-header">
          <h2>Administracija</h2>
          <div class="admin-header-actions">
            <button type="button" class="btn btn-danger" id="admin-exit">Izađi iz programa</button>
            <button type="button" class="btn btn-ghost" id="admin-close">Zatvori</button>
          </div>
        </div>
        <div class="admin-tabs" id="admin-tabs">
          <button type="button" class="admin-tab" data-tab="prices">Cene</button>
          <button type="button" class="admin-tab" data-tab="sales">Prodaje / Z-izveštaj</button>
          <button type="button" class="admin-tab" data-tab="devices">Uređaji</button>
          <button type="button" class="admin-tab" data-tab="pin">PIN</button>
          <button type="button" class="admin-tab" data-tab="export">Izvoz</button>
        </div>
        <div class="admin-content" id="admin-content"></div>
      </div>
    </div>
  `;

  const backdrop = root.querySelector<HTMLElement>("#admin-backdrop")!;
  const closeBtn = root.querySelector<HTMLButtonElement>("#admin-close")!;
  const exitBtn = root.querySelector<HTMLButtonElement>("#admin-exit")!;
  const tabsEl = root.querySelector<HTMLElement>("#admin-tabs")!;
  const contentEl = root.querySelector<HTMLElement>("#admin-content")!;

  closeBtn.addEventListener("click", () => closeAdmin(root));

  // Two-click confirm so a mis-tap never quits the kiosk. PIN re-verified server-side.
  let armed = false;
  let armTimer = 0;
  exitBtn.addEventListener("click", async () => {
    if (!armed) {
      armed = true;
      exitBtn.textContent = "Potvrdi izlaz?";
      armTimer = window.setTimeout(() => {
        armed = false;
        exitBtn.textContent = "Izađi iz programa";
      }, 3000);
      return;
    }
    window.clearTimeout(armTimer);
    exitBtn.disabled = true;
    exitBtn.textContent = "Izlazak...";
    try {
      await adminExit(pin);
    } catch (err) {
      exitBtn.disabled = false;
      armed = false;
      exitBtn.textContent = `Greška: ${String(err)}`;
    }
  });
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) closeAdmin(root);
  });

  function selectTab(tab: Tab): void {
    for (const btn of tabsEl.querySelectorAll<HTMLElement>(".admin-tab")) {
      btn.classList.toggle("admin-tab-active", btn.dataset.tab === tab);
    }
    switch (tab) {
      case "prices": return void renderPrices(contentEl);
      case "sales": return void renderSales(contentEl);
      case "devices": return void renderDevices(contentEl);
      case "pin": return void renderPin(contentEl);
      case "export": return void renderExport(contentEl);
    }
  }

  tabsEl.addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>("[data-tab]");
    if (btn) selectTab(btn.dataset.tab as Tab);
  });

  selectTab("prices");
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

async function renderPrices(el: HTMLElement): Promise<void> {
  el.innerHTML = `<p class="admin-loading">Učitavanje...</p>`;
  let settings: Settings;
  try {
    settings = await adminGetSettings();
  } catch (err) {
    el.innerHTML = `<p class="admin-error">Greška: ${escapeHtml(String(err))}</p>`;
    return;
  }

  el.innerHTML = `
    <table class="admin-table">
      <thead><tr><th>Tip karte</th><th>Cena (RSD)</th></tr></thead>
      <tbody id="price-rows">
        ${settings.ticket_types
          .map(
            (t) => `
          <tr>
            <td>${escapeHtml(t.label)}</td>
            <td><input type="number" step="100" min="0" class="admin-input" data-code="${escapeHtml(t.code)}" value="${t.price_rsd}" /></td>
          </tr>`
          )
          .join("")}
      </tbody>
    </table>
    <div class="admin-actions">
      <button type="button" class="btn btn-primary" id="save-prices">Sačuvaj</button>
      <span class="admin-status" id="price-status"></span>
    </div>
  `;

  const statusEl = el.querySelector<HTMLElement>("#price-status")!;
  el.querySelector<HTMLButtonElement>("#save-prices")!.addEventListener("click", async () => {
    const inputs = el.querySelectorAll<HTMLInputElement>("[data-code]");
    const types: TicketType[] = settings.ticket_types.map((t) => {
      const input = Array.from(inputs).find((i) => i.dataset.code === t.code);
      const price = input ? Number(input.value) : t.price_rsd;
      return { ...t, price_rsd: Number.isFinite(price) ? price : t.price_rsd };
    });
    statusEl.textContent = "Čuvanje...";
    try {
      await adminSetPrices(types);
      statusEl.textContent = "Sačuvano.";
    } catch (err) {
      statusEl.textContent = `Greška: ${String(err)}`;
    }
  });
}

function saleStatus(s: SaleRecord): string {
  if (s.status === "paid") {
    return s.printed ? "Plaćeno" : `<span class="status-warn">Plaćeno · nije štampano</span>`;
  }
  if (s.status === "abandoned") return `<span class="status-warn">Napušteno</span>`;
  return "U toku";
}

async function renderSales(el: HTMLElement): Promise<void> {
  const { from, to } = todayRange();
  el.innerHTML = `<p class="admin-loading">Učitavanje...</p>`;

  let sales: SaleRecord[];
  let report: ZReport;
  try {
    [sales, report] = await Promise.all([adminListSales(from, to), adminZReport(from, to)]);
  } catch (err) {
    el.innerHTML = `<p class="admin-error">Greška: ${escapeHtml(String(err))}</p>`;
    return;
  }

  el.innerHTML = `
    <div class="zreport">
      <h3>Z-izveštaj (danas)</h3>
      <div class="zreport-summary">
        <div><span class="zreport-value">${report.count_sales}</span><span class="zreport-label">prodaja</span></div>
        <div><span class="zreport-value">${report.count_tickets}</span><span class="zreport-label">karata</span></div>
        <div><span class="zreport-value">${report.total_rsd}</span><span class="zreport-label">RSD naplaćeno</span></div>
        <div><span class="zreport-value">${report.abandoned_rsd}</span><span class="zreport-label">RSD napušteno</span></div>
      </div>
      <table class="admin-table admin-table-compact">
        <thead><tr><th>Tip</th><th>Kom.</th><th>RSD</th></tr></thead>
        <tbody>
          ${report.by_type
            .map(([code, qty, amount]) => `<tr><td>${escapeHtml(code)}</td><td>${qty}</td><td>${amount}</td></tr>`)
            .join("")}
        </tbody>
      </table>
    </div>
    <h3>Prodaje danas</h3>
    <table class="admin-table" id="sales-table">
      <thead><tr><th>Vreme</th><th>Status</th><th>Karata</th><th>Iznos</th><th>Reprint</th><th></th></tr></thead>
      <tbody>
        ${sales
          .map(
            (s) => `
          <tr data-sale="${escapeHtml(s.id)}">
            <td>${escapeHtml(new Date(s.created_at * 1000).toLocaleTimeString("sr-RS"))}</td>
            <td>${saleStatus(s)}</td>
            <td>${s.num_tickets}</td>
            <td>${s.status === "abandoned" ? `${s.inserted_rsd} / ${s.total_rsd}` : s.total_rsd} RSD</td>
            <td>${s.reprinted_count}</td>
            <td>${
              s.status === "paid"
                ? `<button type="button" class="btn btn-ghost btn-small" data-reprint="${escapeHtml(s.id)}">Reprint</button>`
                : ""
            }</td>
          </tr>`
          )
          .join("")}
      </tbody>
    </table>
    <p class="admin-status" id="reprint-status"></p>
  `;

  const reprintStatus = el.querySelector<HTMLElement>("#reprint-status")!;
  el.querySelector<HTMLElement>("#sales-table")!.addEventListener("click", async (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-reprint]");
    if (!btn) return;
    const saleId = btn.dataset.reprint!;
    btn.disabled = true;
    const original = btn.textContent;
    btn.textContent = "...";
    reprintStatus.textContent = "";
    try {
      await adminReprint(saleId);
      btn.textContent = "Odštampano";
    } catch (err) {
      btn.textContent = original;
      btn.disabled = false;
      reprintStatus.textContent = `Greška pri reprintu: ${String(err)}`;
    }
  });
}

async function renderDevices(el: HTMLElement): Promise<void> {
  el.innerHTML = `<p class="admin-loading">Provera uređaja...</p>`;
  let status: DeviceStatus;
  let settings: Settings;
  try {
    [status, settings] = await Promise.all([deviceStatus(), adminGetSettings()]);
  } catch (err) {
    el.innerHTML = `<p class="admin-error">Greška: ${escapeHtml(String(err))}</p>`;
    return;
  }
  const version = await getVersion().catch(() => "?");
  el.innerHTML = `
    <div class="cfg-hint">Verzija aplikacije: <b>${escapeHtml(version)}</b></div>
    <div class="device-row">
      <span class="device-dot ${status.validator_connected ? "device-ok" : "device-bad"}"></span>
      <div>
        <div class="device-name">NV9 validator</div>
        <div class="device-detail">${escapeHtml(status.validator_detail || (status.validator_connected ? "Povezan" : "Nije povezan"))}</div>
      </div>
    </div>
    <div class="device-row">
      <span class="device-dot ${status.printer_connected ? "device-ok" : "device-bad"}"></span>
      <div>
        <div class="device-name">Štampač</div>
        <div class="device-detail">${escapeHtml(status.printer_detail || (status.printer_connected ? "Povezan" : "Nije povezan"))}</div>
      </div>
    </div>
    <div class="pin-form">
      <label>NV9 COM port (podrazumevano COM3)<input type="text" id="cfg-nv9" class="admin-input" placeholder="COM3" value="${escapeHtml(settings.nv9_port ?? "COM3")}" /></label>
      <label>Printer — Windows ime (najpouzdanije; prazno = koristi COM/USB)<input type="text" id="cfg-win" class="admin-input" placeholder="BIXOLON SRP-Q300" value="${escapeHtml(settings.printer_windows_name ?? "")}" /></label>
      <label>Printer COM port (koristi se ako je Windows ime prazno)<input type="text" id="cfg-printer" class="admin-input" placeholder="COM4" value="${escapeHtml(settings.printer_port ?? "")}" /></label>
      <label class="cfg-check"><input type="checkbox" id="cfg-simple" ${settings.simple_mode ? "checked" : ""} /> Jednostavni režim (bez touch-a — uvek karta za odrasle)</label>
      <label>Feed pre reza (mm)<input type="number" id="cfg-fb" class="admin-input" min="0" max="200" value="${settings.feed_before_cut_mm}" /></label>
      <label>Feed posle reza / rep (mm)<input type="number" id="cfg-fa" class="admin-input" min="0" max="200" value="${settings.feed_after_cut_mm}" /></label>
      <label>Širina papira (mm) — npr. 80, 58, ili uže<input type="number" id="cfg-pw" class="admin-input" min="28" max="120" value="${settings.paper_width_mm}" /></label>
      <div class="admin-actions">
        <button type="button" class="btn btn-primary" id="save-devices">Sačuvaj</button>
        <button type="button" class="btn btn-ghost" id="test-print">Test štampa</button>
        <button type="button" class="btn btn-ghost" id="refresh-devices">Osveži status</button>
        <span class="admin-status" id="dev-status"></span>
      </div>
      <p class="cfg-hint">Portovi se primenjuju na sledeću transakciju. Promena režima traži restart aplikacije.</p>
    </div>
  `;
  const devStatus = el.querySelector<HTMLElement>("#dev-status")!;
  el.querySelector<HTMLButtonElement>("#refresh-devices")!.addEventListener("click", () => void renderDevices(el));
  el.querySelector<HTMLButtonElement>("#test-print")!.addEventListener("click", async () => {
    devStatus.textContent = "Štampam test kartu...";
    try {
      await adminTestPrint();
      devStatus.textContent = "Test karta poslata na štampač.";
    } catch (err) {
      devStatus.textContent = `Greška: ${String(err)}`;
    }
  });
  el.querySelector<HTMLButtonElement>("#save-devices")!.addEventListener("click", async () => {
    const nv9 = el.querySelector<HTMLInputElement>("#cfg-nv9")!.value;
    const printer = el.querySelector<HTMLInputElement>("#cfg-printer")!.value;
    const win = el.querySelector<HTMLInputElement>("#cfg-win")!.value;
    const simple = el.querySelector<HTMLInputElement>("#cfg-simple")!.checked;
    const fb = Number(el.querySelector<HTMLInputElement>("#cfg-fb")!.value) || 0;
    const fa = Number(el.querySelector<HTMLInputElement>("#cfg-fa")!.value) || 0;
    const pw = Number(el.querySelector<HTMLInputElement>("#cfg-pw")!.value) || 80;
    devStatus.textContent = "Čuvanje...";
    try {
      await adminSetDevices(nv9, printer, win, fb, fa, pw);
      await adminSetSimpleMode(simple);
      devStatus.textContent = "Sačuvano.";
    } catch (err) {
      devStatus.textContent = `Greška: ${String(err)}`;
    }
  });
}

function renderPin(el: HTMLElement): void {
  el.innerHTML = `
    <div class="pin-form">
      <label>Trenutni PIN<input type="password" id="old-pin" class="admin-input" /></label>
      <label>Novi PIN<input type="password" id="new-pin" class="admin-input" /></label>
      <label>Potvrda novog PIN-a<input type="password" id="confirm-pin" class="admin-input" /></label>
      <div class="admin-actions">
        <button type="button" class="btn btn-primary" id="change-pin">Promeni PIN</button>
        <span class="admin-status" id="pin-status"></span>
      </div>
    </div>
  `;
  const statusEl = el.querySelector<HTMLElement>("#pin-status")!;
  el.querySelector<HTMLButtonElement>("#change-pin")!.addEventListener("click", async () => {
    const oldPin = el.querySelector<HTMLInputElement>("#old-pin")!.value;
    const newPin = el.querySelector<HTMLInputElement>("#new-pin")!.value;
    const confirmPin = el.querySelector<HTMLInputElement>("#confirm-pin")!.value;
    if (!newPin || newPin !== confirmPin) {
      statusEl.textContent = "Novi PIN i potvrda se ne poklapaju.";
      return;
    }
    statusEl.textContent = "Čuvanje...";
    try {
      await adminChangePin(oldPin, newPin);
      statusEl.textContent = "PIN promenjen.";
    } catch (err) {
      statusEl.textContent = `Greška: ${String(err)}`;
    }
  });
}

function renderExport(el: HTMLElement): void {
  const { from, to } = todayRange();
  const toDateInput = (ts: number): string => new Date(ts * 1000).toISOString().slice(0, 10);

  el.innerHTML = `
    <div class="export-form">
      <label>Od<input type="date" id="export-from" class="admin-input" value="${toDateInput(from)}" /></label>
      <label>Do<input type="date" id="export-to" class="admin-input" value="${toDateInput(to)}" /></label>
      <div class="admin-actions">
        <button type="button" class="btn btn-primary" id="run-export">Izvezi CSV</button>
        <span class="admin-status" id="export-status"></span>
      </div>
      <textarea id="export-output" class="export-output" readonly placeholder="CSV izlaz će se prikazati ovde (kopirajte ručno — nema fajl-dijaloga u kiosk kapabilitetima)."></textarea>
    </div>
  `;

  const statusEl = el.querySelector<HTMLElement>("#export-status")!;
  const outputEl = el.querySelector<HTMLTextAreaElement>("#export-output")!;

  el.querySelector<HTMLButtonElement>("#run-export")!.addEventListener("click", async () => {
    const fromVal = el.querySelector<HTMLInputElement>("#export-from")!.value;
    const toVal = el.querySelector<HTMLInputElement>("#export-to")!.value;
    const fromTs = Math.floor(new Date(`${fromVal}T00:00:00`).getTime() / 1000);
    const toTs = Math.floor(new Date(`${toVal}T23:59:59`).getTime() / 1000);
    statusEl.textContent = "Izvoz...";
    try {
      const csv = await adminExportCsv(fromTs, toTs);
      outputEl.value = csv;
      statusEl.textContent = "Gotovo.";
    } catch (err) {
      statusEl.textContent = `Greška: ${String(err)}`;
    }
  });
}
