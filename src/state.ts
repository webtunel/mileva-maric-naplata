// Central app state + tiny pub/sub store. No framework — screens subscribe
// and re-render themselves on change (see main.ts for the mount/update loop).

import type { PrintedTicket, Settings } from "./api";

export enum Screen {
  Welcome = "welcome",
  Select = "select",
  Pay = "pay",
  Success = "success",
  Simple = "simple",
}

/** Ticket qty keyed by TicketType.code. Absent key == 0. */
export type CartState = Record<string, number>;

export interface AppState {
  screen: Screen;
  config: Settings | null;
  cart: CartState;
  saleId: string | null;
  tickets: PrintedTicket[];
  paymentTotal: number;
  paymentInserted: number;
  paymentActive: boolean;
  lastInserted: number; // amount actually taken in the finished sale (for success screen)
  lastTotal: number; // price of the finished sale
  toast: string | null;
}

type Listener = (state: AppState) => void;

const state: AppState = {
  screen: Screen.Welcome,
  config: null,
  cart: {},
  saleId: null,
  tickets: [],
  paymentTotal: 0,
  paymentInserted: 0,
  paymentActive: false,
  lastInserted: 0,
  lastTotal: 0,
  toast: null,
};

const listeners: Listener[] = [];

export function subscribe(fn: Listener): () => void {
  listeners.push(fn);
  return () => {
    const i = listeners.indexOf(fn);
    if (i >= 0) listeners.splice(i, 1);
  };
}

export function getState(): AppState {
  return state;
}

// Reentrancy guard: screen unmount handlers call setState from INSIDE a render pass
// (e.g. pay.ts clears paymentActive on unmount). Recursing into the listeners there
// caused an infinite render loop → JS stack overflow → the app froze right after
// payment. Instead, nested setState calls just mark the state dirty and the outer
// notification loop re-runs once.
let notifying = false;
let dirty = false;

export function setState(patch: Partial<AppState>): void {
  Object.assign(state, patch);
  if (notifying) {
    dirty = true;
    return;
  }
  notifying = true;
  try {
    do {
      dirty = false;
      for (const l of listeners) l(state);
    } while (dirty);
  } finally {
    notifying = false;
  }
}

/** Total number of tickets currently in the cart. */
export function totalQty(cart: CartState): number {
  let sum = 0;
  for (const qty of Object.values(cart)) sum += qty;
  return sum;
}

/** Total price (RSD) of the cart against the current price list. */
export function total(cart: CartState, config: Settings | null): number {
  if (!config) return 0;
  let sum = 0;
  for (const t of config.ticket_types) {
    sum += (cart[t.code] ?? 0) * t.price_rsd;
  }
  return sum;
}

/** Cart -> the shape `start_payment` expects, dropping zero-qty lines. */
export function toApiCart(cart: CartState): { lines: { code: string; qty: number }[] } {
  return {
    lines: Object.entries(cart)
      .filter(([, qty]) => qty > 0)
      .map(([code, qty]) => ({ code, qty })),
  };
}

export function resetTransaction(): void {
  setState({
    screen: Screen.Welcome,
    cart: {},
    saleId: null,
    tickets: [],
    paymentTotal: 0,
    paymentInserted: 0,
  });
}

let toastTimer: number | undefined;

export function showToast(message: string, ms = 4000): void {
  window.clearTimeout(toastTimer);
  setState({ toast: message });
  toastTimer = window.setTimeout(() => setState({ toast: null }), ms);
}

/** Minimal HTML escaping for values interpolated into template strings. */
export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
