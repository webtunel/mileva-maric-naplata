import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getState, Screen } from "./state";

// Kiosk auto-update: silently check GitHub Releases for a newer *signed* build, install
// it, then relaunch. Failures (offline, no endpoint, plain browser) are ignored so the
// kiosk keeps running no matter what.
async function applyUpdateIfAny(force: boolean): Promise<void> {
  try {
    const update = await check();
    if (!update) return;
    // Never interrupt a transaction: only update at startup (force) or while idle on the
    // welcome screen. In simple/other screens, the update applies on the next restart.
    if (!force && getState().screen !== Screen.Welcome) return;
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
