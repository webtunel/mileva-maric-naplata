import { getState, setState, resetTransaction, showToast, escapeHtml, type AppState } from "../state";
import { printTickets, type PrintedTicket } from "../api";
import type { ScreenController } from "./welcome";

function ticketCardHtml(t: PrintedTicket): string {
  const date = new Date(t.issued_at * 1000).toLocaleString("sr-RS");
  const shortCode = t.id.slice(0, 8).toUpperCase();
  return `
    <div class="ticket-card">
      <div class="ticket-card-label">${escapeHtml(t.label)}</div>
      <div class="ticket-card-price">${t.price_rsd} RSD</div>
      <div class="ticket-card-date">${escapeHtml(date)}</div>
      <div class="ticket-card-code">#${escapeHtml(shortCode)}</div>
    </div>
  `;
}

export function mountSuccess(container: HTMLElement): ScreenController {
  container.innerHTML = `
    <div class="screen screen-success">
      <div class="success-check" aria-hidden="true">&#10003;</div>
      <h1 class="title">Uspešno ste platili karte</h1>
      <div class="ticket-cards" id="ticket-cards"></div>
      <div class="success-actions" id="success-actions"></div>
    </div>
  `;

  const cardsEl = container.querySelector<HTMLElement>("#ticket-cards")!;
  const actionsEl = container.querySelector<HTMLElement>("#success-actions")!;

  function renderPrintAction(): void {
    actionsEl.innerHTML = `<button type="button" class="btn btn-primary btn-huge" id="print-btn">Odštampaj ulaznice</button>`;
    const btn = actionsEl.querySelector<HTMLButtonElement>("#print-btn")!;
    btn.addEventListener("click", async () => {
      const saleId = getState().saleId;
      if (!saleId) return;
      btn.disabled = true;
      btn.textContent = "Štampanje...";
      try {
        const tickets = await printTickets(saleId);
        setState({ tickets });
      } catch (err) {
        showToast(`Štampa nije uspela: ${String(err)}`);
        btn.disabled = false;
        btn.textContent = "Odštampaj ulaznice";
      }
    });
  }

  function renderNewTransactionAction(): void {
    actionsEl.innerHTML = `<button type="button" class="btn btn-primary btn-huge" id="new-tx-btn">Nova transakcija</button>`;
    actionsEl.querySelector<HTMLButtonElement>("#new-tx-btn")!.addEventListener("click", () => {
      resetTransaction();
    });
  }

  function update(state: AppState): void {
    cardsEl.innerHTML = state.tickets.map(ticketCardHtml).join("");
    if (state.tickets.length > 0) renderNewTransactionAction();
    else renderPrintAction();
  }

  update(getState());

  return { update };
}
