import { onOverlayLevel, onOverlayState, type OverlayState } from "../shared/ipc";
import "./overlay.css";

const root = document.getElementById("overlay-root")!;
root.innerHTML = `
  <div class="overlay-shell" data-phase="hidden">
    <div class="indicator" data-phase="hidden" aria-hidden="true">
      <span class="waveform">
        <i></i><i></i><i></i><i></i><i></i>
      </span>
      <span class="spinner"></span>
      <span class="error-mark">!</span>
    </div>
  </div>
`;

const shell = root.querySelector<HTMLElement>(".overlay-shell")!;
const indicator = root.querySelector<HTMLElement>(".indicator")!;
const bars = [...root.querySelectorAll<HTMLElement>(".waveform i")];

function render(state: OverlayState): void {
  shell.dataset.phase = state.phase;
  indicator.dataset.phase = state.phase;
}

function setLevel(level: number): void {
  const clamped = Math.max(0, Math.min(1, level));
  const weights = [0.58, 0.82, 1, 0.78, 0.52];
  bars.forEach((bar, index) => {
    const height = 4 + clamped * weights[index] * 16;
    bar.style.height = `${height}px`;
  });
}

void onOverlayState(render);
void onOverlayLevel(setLevel);
