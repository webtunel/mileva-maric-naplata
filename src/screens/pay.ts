import { getState, setState, Screen, total, toApiCart, type AppState } from "../state";
import { startPayment, cancelPayment, onPaymentProgress } from "../api";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { ScreenController } from "./welcome";

export function mountPay(container: HTMLElement): ScreenController {
  container.innerHTML = `
    <div class="screen screen-pay">
      <h1 class="title">Ubacite novčanice</h1>
      <div class="pay-notice">⚠ Automat ne vraća kusur.</div>
      <div class="pay-panels">
        <div class="pay-panel">
          <div class="pay-panel-label">UBAČENO</div>
          <div class="pay-panel-amount" id="inserted-amount">0 RSD</div>
        </div>
        <div class="pay-panel">
          <div class="pay-panel-label">POTREBNO</div>
          <div class="pay-panel-amount" id="needed-amount">0 RSD</div>
        </div>
      </div>
      <div class="pay-progress-track">
        <div class="pay-progress-bar" id="pay-progress-bar"></div>
      </div>
      <div class="pay-status" id="pay-status"></div>
      <button type="button" class="link-cancel" id="cancel-link">Otkaži transakciju</button>
    </div>
  `;

  const insertedEl = container.querySelector<HTMLElement>("#inserted-amount")!;
  const neededEl = container.querySelector<HTMLElement>("#needed-amount")!;
  const barEl = container.querySelector<HTMLElement>("#pay-progress-bar")!;
  const statusEl = container.querySelector<HTMLElement>("#pay-status")!;
  const cancelLink = container.querySelector<HTMLButtonElement>("#cancel-link")!;

  let unlisten: UnlistenFn | null = null;
  let settled = false;
  let returnedStreak = 0;

  const initial = getState();
  const totalAmount = total(initial.cart, initial.config);
  const cart = toApiCart(initial.cart);
  setState({ paymentTotal: totalAmount, paymentInserted: 0, paymentActive: true });

  onPaymentProgress((p) => {
    if (settled) return;
    setState({ paymentInserted: p.inserted_rsd, paymentTotal: p.total_rsd });
    if (p.complete) {
      // Enough money in — the backend is finishing the sale and we're about to switch to
      // the printing screen. Show it so the pay screen never looks stuck at "full".
      statusEl.textContent = "Plaćanje uspešno — priprema računa...";
      statusEl.classList.remove("pay-status-warn");
      return;
    }
    if (p.note) {
      const returned = /vraćena|kusur|apoen/i.test(p.note);
      returnedStreak = returned ? returnedStreak + 1 : 0;
      if (returnedStreak >= 2) {
        statusEl.textContent = "Nemate odgovarajuće apoene za tačan iznos — pozovite osoblje ili otkažite.";
        statusEl.classList.add("pay-status-warn");
      } else {
        statusEl.textContent = p.note;
        statusEl.classList.toggle("pay-status-warn", returned);
      }
    }
  })
    .then((fn) => {
      unlisten = fn;
      if (settled) fn();
    })
    .catch(() => {
      /* event bridge unavailable (e.g. plain browser preview) — progress just stays at 0 */
    });

  startPayment(cart)
    .then((outcome) => {
      if (settled) return;
      settled = true;
      unlisten?.();
      setState({
        screen: Screen.Success,
        saleId: outcome.sale_id,
        tickets: [],
        lastInserted: outcome.inserted_rsd,
        lastTotal: outcome.total_rsd,
      });
    })
    .catch((err) => {
      if (settled) return;
      settled = true;
      unlisten?.();
      // Show the failure ON the pay screen (persistent) so it's diagnosable instead of a
      // silent jump back to selection.
      statusEl.textContent = `Greška pri plaćanju: ${String(err)}`;
      statusEl.classList.add("pay-status-warn");
      window.setTimeout(() => setState({ screen: Screen.Select }), 6000);
    });

  const onCancel = (): void => {
    if (settled) return;
    settled = true;
    unlisten?.();
    cancelPayment().catch(() => {
      /* best effort — we're leaving the screen regardless */
    });
    setState({ screen: Screen.Select });
  };
  cancelLink.addEventListener("click", onCancel);

  function update(state: AppState): void {
    insertedEl.textContent = `${state.paymentInserted} RSD`;
    neededEl.textContent = `${state.paymentTotal} RSD`;
    const pct = state.paymentTotal > 0
      ? Math.min(100, (state.paymentInserted / state.paymentTotal) * 100)
      : 0;
    barEl.style.width = `${pct}%`;
  }

  update(getState());

  return {
    update,
    unmount(): void {
      settled = true;
      setState({ paymentActive: false, paymentInserted: 0 });
      unlisten?.();
      cancelLink.removeEventListener("click", onCancel);
    },
  };
}
