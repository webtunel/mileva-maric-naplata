import { getState, setState, escapeHtml, type AppState } from "../state";
import { startPayment, printTickets, onPaymentProgress } from "../api";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { ScreenController } from "./welcome";

// Touchless simple mode: always one adult ticket. The moment the exact price is
// inserted, the ticket prints and the machine resets for the next visitor. No taps.
export function mountSimple(container: HTMLElement): ScreenController {
  const config = getState().config;
  const adult =
    config?.ticket_types.find((t) => t.code === "odrasli") ?? config?.ticket_types[0];
  const price = adult?.price_rsd ?? 0;
  const label = adult?.label ?? "Odrasli";

  container.innerHTML = `
    <div class="screen screen-simple">
      <div class="simple-title">Kupovina ulaznice</div>
      <div class="simple-price">${price} RSD</div>
      <div class="simple-sub">${escapeHtml(label)} — ubacite tačan iznos</div>
      <div class="pay-notice">⚠ Automat ne vraća kusur.</div>
      <div class="pay-panels">
        <div class="pay-panel"><div class="pay-panel-label">UBAČENO</div><div class="pay-panel-amount" id="s-inserted">0 RSD</div></div>
        <div class="pay-panel"><div class="pay-panel-label">POTREBNO</div><div class="pay-panel-amount" id="s-needed">${price} RSD</div></div>
      </div>
      <div class="pay-progress-track"><div class="pay-progress-bar" id="s-bar"></div></div>
      <div class="pay-status" id="s-status">Ubacite novac...</div>
    </div>
  `;

  const insertedEl = container.querySelector<HTMLElement>("#s-inserted")!;
  const barEl = container.querySelector<HTMLElement>("#s-bar")!;
  const statusEl = container.querySelector<HTMLElement>("#s-status")!;

  let unlisten: UnlistenFn | null = null;
  let stopped = false;
  let returnedStreak = 0;
  let countdownTimer = 0;

  onPaymentProgress((p) => {
    if (stopped) return;
    setState({ paymentInserted: p.inserted_rsd });
    insertedEl.textContent = `${p.inserted_rsd} RSD`;
    const pct = p.total_rsd > 0 ? Math.min(100, (p.inserted_rsd / p.total_rsd) * 100) : 0;
    barEl.style.width = `${pct}%`;
    if (p.note) {
      const returned = /vraćena|kusur|apoen|previše/i.test(p.note);
      returnedStreak = returned ? returnedStreak + 1 : 0;
      statusEl.textContent =
        returnedStreak >= 2 ? "Nemate odgovarajuće apoene — pozovite osoblje." : p.note;
      statusEl.classList.toggle("pay-status-warn", returned);
    }
  })
    .then((fn) => {
      unlisten = fn;
      if (stopped) fn();
    })
    .catch(() => {});

  function resetLine(message: string): void {
    if (stopped) return;
    insertedEl.textContent = "0 RSD";
    barEl.style.width = "0%";
    statusEl.textContent = message;
    statusEl.classList.remove("pay-status-warn");
    returnedStreak = 0;
  }

  async function runOnce(): Promise<void> {
    if (stopped || price <= 0 || !adult) return;
    setState({ paymentActive: true, paymentInserted: 0 });
    try {
      const outcome = await startPayment({ lines: [{ code: adult.code, qty: 1 }] });
      if (stopped) return;
      statusEl.textContent = "Uspešno! Štampam kartu...";
      statusEl.classList.remove("pay-status-warn");
      try {
        await printTickets(outcome.sale_id);
      } catch (err) {
        statusEl.textContent = `Karta plaćena, štampa nije uspela: ${String(err)}`;
      }
      // Transaction fully handled — safe for a pending update to install now.
      setState({ paymentActive: false, paymentInserted: 0 });
      // Count down for the next customer, then auto-start a fresh session.
      let n = 5;
      statusEl.textContent = `Uzmite kartu. Sledeći kupac za ${n}s...`;
      countdownTimer = window.setInterval(() => {
        if (stopped) {
          window.clearInterval(countdownTimer);
          return;
        }
        n -= 1;
        if (n <= 0) {
          window.clearInterval(countdownTimer);
          resetLine("Ubacite novac...");
          void runOnce();
          return;
        }
        statusEl.textContent = `Uzmite kartu. Sledeći kupac za ${n}s...`;
      }, 1000);
    } catch {
      // cancel / timeout / hardware error — reset and keep waiting
      setState({ paymentActive: false, paymentInserted: 0 });
      resetLine("Ubacite novac...");
      window.setTimeout(() => {
        if (!stopped) void runOnce();
      }, 2000);
    }
  }

  if (price <= 0 || !adult) {
    statusEl.textContent = "Cena karte za odrasle nije podešena (otvorite admin).";
  } else {
    void runOnce();
  }

  return {
    update(_state: AppState): void {},
    unmount(): void {
      stopped = true;
      setState({ paymentActive: false, paymentInserted: 0 });
      window.clearInterval(countdownTimer);
      unlisten?.();
    },
  };
}
