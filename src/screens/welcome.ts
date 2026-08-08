import { Screen, setState } from "../state";

export interface ScreenController {
  update?: (state: import("../state").AppState) => void;
  unmount?: () => void;
}

export function mountWelcome(container: HTMLElement): ScreenController {
  container.innerHTML = `
    <div class="screen screen-welcome">
      <h1 class="title title-huge">Dobrodošli u Muzej Mileve Marić</h1>
      <p class="subtitle">
        Dodirnite dugme ispod da kupite ulaznice za posetu muzeju.
      </p>
      <button type="button" class="btn btn-primary btn-huge" id="start-btn">
        Kupite kartu za ulazak u muzej
      </button>
    </div>
  `;

  const startBtn = container.querySelector<HTMLButtonElement>("#start-btn")!;
  const onClick = (): void => setState({ screen: Screen.Select });
  startBtn.addEventListener("click", onClick);

  return {
    unmount(): void {
      startBtn.removeEventListener("click", onClick);
    },
  };
}
