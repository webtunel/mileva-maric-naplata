import { getState, setState, subscribe, Screen, type AppState } from "./state";
import { getConfig } from "./api";
import { mountWelcome, type ScreenController } from "./screens/welcome";
import { mountSelect } from "./screens/select";
import { mountPay } from "./screens/pay";
import { mountSuccess } from "./screens/success";
import { mountSimple } from "./screens/simple";
import { initAdmin } from "./admin";
import { startAutoUpdate } from "./updater";

let current: ScreenController | null = null;
let currentScreen: Screen | null = null;

function mountScreen(screen: Screen, container: HTMLElement): ScreenController {
  switch (screen) {
    case Screen.Welcome:
      return mountWelcome(container);
    case Screen.Select:
      return mountSelect(container);
    case Screen.Pay:
      return mountPay(container);
    case Screen.Success:
      return mountSuccess(container);
    case Screen.Simple:
      return mountSimple(container);
  }
}

function renderToast(state: AppState): void {
  const toastEl = document.getElementById("toast");
  if (!toastEl) return;
  if (state.toast) {
    toastEl.textContent = state.toast;
    toastEl.classList.remove("toast-hidden");
  } else {
    toastEl.classList.add("toast-hidden");
  }
}

function render(state: AppState): void {
  const outlet = document.getElementById("screen");
  if (!outlet) return;

  if (state.screen !== currentScreen) {
    // Commit the new screen BEFORE unmounting: unmount handlers may call setState,
    // and a stale currentScreen would re-enter this branch (double unmount/mount).
    currentScreen = state.screen;
    const previous = current;
    current = null;
    previous?.unmount?.();
    outlet.innerHTML = "";
    current = mountScreen(state.screen, outlet);
  }
  current?.update?.(state);
  renderToast(state);
}

function startClock(): void {
  const clockEl = document.getElementById("clock");
  if (!clockEl) return;
  const tick = (): void => {
    clockEl.textContent = new Date().toLocaleTimeString("sr-RS", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  };
  tick();
  window.setInterval(tick, 1000);
}

async function bootstrap(): Promise<void> {
  subscribe(render);
  render(getState());
  startClock();

  const hotzone = document.getElementById("admin-hotzone");
  const adminRoot = document.getElementById("admin-root");
  if (hotzone && adminRoot) initAdmin(hotzone, adminRoot);

  startAutoUpdate();

  try {
    const config = await getConfig();
    // Simple (touchless) mode jumps straight to the auto-selling screen.
    setState(config.simple_mode ? { config, screen: Screen.Simple } : { config });
  } catch (err) {
    // Not fatal — screens degrade gracefully without config (e.g. dev preview
    // in a plain browser with no Tauri bridge). Surface it non-blockingly.
    setState({ toast: `Konfiguracija nije učitana: ${String(err)}` });
  }
}

void bootstrap();
