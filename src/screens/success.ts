import { getState, setState, resetTransaction, escapeHtml, type AppState } from "../state";
import { printTickets, type PrintedTicket } from "../api";
import type { ScreenController } from "./welcome";

const NEXT_CUSTOMER_SECONDS = 20;

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
      <h1 class="title">Uspešno! Uzmite kartu ispod.</h1>
      <div class="ticket-cards" id="ticket-cards"></div>
      <div class="pay-status" id="success-status">Štampam kartu...</div>
      <div class="success-actions" id="success-actions"></div>
    </div>
  `;

  const cardsEl = container.querySelector<HTMLElement>("#ticket-cards")!;
  const statusEl = container.querySelector<HTMLElement>("#success-status")!;
  const actionsEl = container.querySelector<HTMLElement>("#success-actions")!;

  let countdown = 0;
  let stopped = false;

  function startCountdown(): void {
    let n = NEXT_CUSTOMER_SECONDS;
    statusEl.textContent = `Uzmite kartu. Sledeći kupac za ${n}s...`;
    actionsEl.innerHTML = `<button type="button" class="btn btn-ghost" id="new-tx-btn">Nova transakcija</button>`;
    actionsEl.querySelector<HTMLButtonElement>("#new-tx-btn")!.addEventListener("click", () => {
      window.clearInterval(countdown);
      resetTransaction();
    });
    countdown = window.setInterval(() => {
      if (stopped) return;
      n -= 1;
      if (n <= 0) {
        window.clearInterval(countdown);
        resetTransaction();
        return;
      }
      statusEl.textContent = `Uzmite kartu. Sledeći kupac za ${n}s...`;
    }, 1000);
  }

  async function autoPrint(): Promise<void> {
    const saleId = getState().saleId;
    if (!saleId) {
      startCountdown();
      return;
    }
    try {
      const tickets = await printTickets(saleId);
      setState({ tickets });
      startCountdown();
    } catch (err) {
      // Sale is recorded and reprintable from admin — don't auto-advance on a print
      // failure; let staff retry so no visitor leaves without a ticket.
      statusEl.textContent = `Štampa nije uspela: ${String(err)}. Pozovite osoblje.`;
      statusEl.classList.add("pay-status-warn");
      actionsEl.innerHTML = `
        <button type="button" class="btn btn-primary" id="retry-btn">Pokušaj ponovo</button>
        <button type="button" class="btn btn-ghost" id="skip-btn">Nova transakcija</button>
      `;
      actionsEl.querySelector<HTMLButtonElement>("#retry-btn")!.addEventListener("click", () => {
        statusEl.classList.remove("pay-status-warn");
        statusEl.textContent = "Štampam kartu...";
        void autoPrint();
      });
      actionsEl.querySelector<HTMLButtonElement>("#skip-btn")!.addEventListener("click", () => resetTransaction());
    }
  }

  function update(state: AppState): void {
    cardsEl.innerHTML = state.tickets.map(ticketCardHtml).join("");
  }

  update(getState());
  void autoPrint();

  return {
    update,
    unmount(): void {
      stopped = true;
      window.clearInterval(countdown);
    },
  };
}
