import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getState } from "./state";

// Kiosk auto-update: silently check GitHub Releases for a newer *signed* build, install
// it, then relaunch. Failures (offline, no endpoint, plain browser) are ignored so the
// kiosk keeps running no matter what.
async function applyUpdateIfAny(force: boolean): Promise<void> {
  try {
    const update = await check();
    if (!update) return;
    // Never interrupt a transaction with cash already in: install at startup (force) or
    // whenever no money is currently inserted (idle between customers, both modes).
    const s = getState();
    if (!force && s.paymentActive && s.paymentInserted > 0) return;
    await update.downloadAndInstall();
    await relaunch();
  } catch (err) {
    console.warn("auto-update preskočen:", err);
  }
}

export function startAutoUpdate(): void {
  // App just launched — safe to update immediately (no transaction in progress).
  void applyUpdateIfAny(true);
  // Re-check periodically; installs only when idle.
  window.setInterval(() => void applyUpdateIfAny(false), 5 * 60 * 1000);
}
