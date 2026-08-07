import { getState, setState, Screen, total, totalQty, escapeHtml, type AppState, type CartState } from "../state";
import type { TicketType } from "../api";
import type { ScreenController } from "./welcome";

export function mountSelect(container: HTMLElement): ScreenController {
  container.innerHTML = `
    <div class="screen screen-select">
      <button type="button" class="btn btn-ghost btn-back" id="back-btn">&larr; Nazad</button>
      <h1 class="title">Izaberite broj karata</h1>
      <p class="select-max" id="select-max"></p>
      <div class="ticket-list" id="ticket-list"></div>
      <div class="select-footer">
        <div class="select-total">
          Ukupno za plaćanje <span id="total-amount">0</span> RSD
          <span class="select-nochange">Automat ne vraća kusur — plaćanje tačnim iznosom</span>
        </div>
        <button type="button" class="btn btn-primary btn-huge" id="ok-btn" disabled>OK</button>
      </div>
    </div>
  `;

  const backBtn = container.querySelector<HTMLButtonElement>("#back-btn")!;
  const maxEl = container.querySelector<HTMLElement>("#select-max")!;
  const list = container.querySelector<HTMLElement>("#ticket-list")!;
  const totalEl = container.querySelector<HTMLElement>("#total-amount")!;
  const okBtn = container.querySelector<HTMLButtonElement>("#ok-btn")!;

  function changeQty(code: string, delta: number): void {
    const state = getState();
    if (!state.config) return;
    const current = state.cart[code] ?? 0;
    if (delta > 0 && totalQty(state.cart) >= state.config.max_total_tickets) return;
    const next = Math.max(0, current + delta);
    const cart: CartState = { ...state.cart, [code]: next };
    setState({ cart });
  }

  const onListClick = (e: MouseEvent): void => {
    const target = e.target as HTMLElement;
    const plus = target.closest<HTMLElement>("[data-plus]");
    const minus = target.closest<HTMLElement>("[data-minus]");
    if (plus) changeQty(plus.dataset.plus!, 1);
    else if (minus) changeQty(minus.dataset.minus!, -1);
  };
  list.addEventListener("click", onListClick);

  const onBack = (): void => setState({ screen: Screen.Welcome });
  backBtn.addEventListener("click", onBack);

  const onOk = (): void => {
    if (totalQty(getState().cart) === 0) return;
    setState({ screen: Screen.Pay });
  };
  okBtn.addEventListener("click", onOk);

  function rowHtml(t: TicketType, qty: number): string {
    return `
      <div class="ticket-row">
        <div class="ticket-row-info">
          <div class="ticket-row-label">${escapeHtml(t.label)}</div>
          <div class="ticket-row-price">${t.price_rsd} RSD po karti</div>
        </div>
        <div class="qty-control">
          <button type="button" class="qty-btn" data-minus="${escapeHtml(t.code)}" ${qty === 0 ? "disabled" : ""} aria-label="Manje">&minus;</button>
          <span class="qty-value">${qty}</span>
          <button type="button" class="qty-btn" data-plus="${escapeHtml(t.code)}" aria-label="Vise">&plus;</button>
        </div>
      </div>
    `;
  }

  function update(state: AppState): void {
    const types = state.config?.ticket_types ?? [];
    maxEl.textContent = state.config
      ? `Maksimalno ${state.config.max_total_tickets} karata po transakciji`
      : "";
    list.innerHTML = types.length
      ? types.map((t) => rowHtml(t, state.cart[t.code] ?? 0)).join("")
      : `<p class="select-empty">Konfiguracija nije dostupna.</p>`;

    const sum = total(state.cart, state.config);
    const qty = totalQty(state.cart);
    totalEl.textContent = String(sum);
    okBtn.disabled = qty === 0;
  }

  update(getState());

  return {
    update,
    unmount(): void {
      list.removeEventListener("click", onListClick);
      backBtn.removeEventListener("click", onBack);
      okBtn.removeEventListener("click", onOk);
    },
  };
}
