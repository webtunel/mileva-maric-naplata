// Thin wrappers over the Tauri v2 bridge. Types mirror src-tauri/src/models.rs
// 1:1 — keep them in sync if the Rust side changes.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Shared domain types (src-tauri/src/models.rs)
// ---------------------------------------------------------------------------

export interface TicketType {
  code: string;
  label: string;
  price_rsd: number;
}

export interface CartLine {
  code: string;
  qty: number;
}

export interface Cart {
  lines: CartLine[];
}

export interface PrintedTicket {
  id: string;
  type_code: string;
  label: string;
  price_rsd: number;
  issued_at: number; // unix seconds
  qr_token: string;
}

export interface SaleRecord {
  id: string;
  created_at: number; // unix seconds
  total_rsd: number;
  inserted_rsd: number;
  num_tickets: number;
  tickets: PrintedTicket[];
  reprinted_count: number;
  status: string; // "pending" | "paid" | "abandoned"
  printed: boolean;
}

export interface PaymentProgress {
  inserted_rsd: number;
  total_rsd: number;
  complete: boolean;
  note: string | null;
}

export interface PaymentOutcome {
  sale_id: string;
  inserted_rsd: number;
  total_rsd: number;
}

export interface DeviceStatus {
  validator_connected: boolean;
  validator_detail: string;
  printer_connected: boolean;
  printer_detail: string;
}

export interface Settings {
  ticket_types: TicketType[];
  max_total_tickets: number;
  museum_name: string;
  nv9_port: string | null;
  printer_vendor_id: number | null;
  printer_product_id: number | null;
  printer_port: string | null;
  printer_windows_name: string | null;
  simple_mode: boolean;
  feed_before_cut_mm: number;
  feed_after_cut_mm: number;
  paper_width_mm: number;
}

export interface ZReport {
  from_ts: number;
  to_ts: number;
  count_sales: number;
  count_tickets: number;
  total_rsd: number;
  abandoned_rsd: number; // partial cash from abandoned sessions (reconciliation)
  by_type: [string, number, number][]; // (type_code, qty, amount_rsd)
}

// ---------------------------------------------------------------------------
// Kiosk commands
// ---------------------------------------------------------------------------

export function getConfig(): Promise<Settings> {
  return invoke<Settings>("get_config");
}

/** Resolves once the visitor has paid in full (or rejects on cancel/error). */
export function startPayment(cart: Cart): Promise<PaymentOutcome> {
  return invoke<PaymentOutcome>("start_payment", { cart });
}

export function cancelPayment(): Promise<void> {
  return invoke<void>("cancel_payment");
}

export function printTickets(saleId: string): Promise<PrintedTicket[]> {
  return invoke<PrintedTicket[]>("print_tickets", { saleId });
}

export function deviceStatus(): Promise<DeviceStatus> {
  return invoke<DeviceStatus>("device_status");
}

/** Subscribe to live NV9 progress. Resolves with the unlisten function. */
export function onPaymentProgress(handler: (p: PaymentProgress) => void): Promise<UnlistenFn> {
  return listen<PaymentProgress>("payment://progress", (event) => handler(event.payload));
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

export function adminLogin(pin: string): Promise<boolean> {
  return invoke<boolean>("admin_login", { pin });
}

export function adminGetSettings(): Promise<Settings> {
  return invoke<Settings>("admin_get_settings");
}

export function adminSetPrices(types: TicketType[]): Promise<void> {
  return invoke<void>("admin_set_prices", { ticketTypes: types });
}

export function adminListSales(from: number, to: number): Promise<SaleRecord[]> {
  return invoke<SaleRecord[]>("admin_list_sales", { fromTs: from, toTs: to });
}

export function adminZReport(from: number, to: number): Promise<ZReport> {
  return invoke<ZReport>("admin_zreport", { fromTs: from, toTs: to });
}

export function adminReprint(saleId: string): Promise<void> {
  return invoke<void>("admin_reprint", { saleId });
}

export function adminChangePin(oldPin: string, newPin: string): Promise<void> {
  return invoke<void>("admin_change_pin", { oldPin, newPin });
}

export function adminExportCsv(from: number, to: number): Promise<string> {
  return invoke<string>("admin_export_csv", { fromTs: from, toTs: to });
}

/** Verifies the PIN and quits the kiosk app (maintenance exit). */
export function adminExit(pin: string): Promise<void> {
  return invoke<void>("admin_exit", { pin });
}

/** Sets device ports + Windows printer name + print feed/width. Empty string -> null. */
export function adminSetDevices(
  nv9Port: string,
  printerPort: string,
  printerWindowsName: string,
  feedBeforeMm: number,
  feedAfterMm: number,
  paperWidthMm: number
): Promise<void> {
  return invoke<void>("admin_set_devices", {
    nv9Port,
    printerPort,
    printerWindowsName,
    feedBeforeMm,
    feedAfterMm,
    paperWidthMm,
  });
}

/** Toggles the touchless single-adult-ticket simple mode. */
export function adminSetSimpleMode(enabled: boolean): Promise<void> {
  return invoke<void>("admin_set_simple_mode", { enabled });
}
