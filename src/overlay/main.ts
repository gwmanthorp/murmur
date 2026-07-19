import { commands, onOverlayLevel, onOverlayState, type OverlayState } from "../shared/ipc";
import "./overlay.css";

const root = document.getElementById("overlay-root")!;
root.innerHTML = `
  <section class="overlay-shell" data-phase="hidden" aria-live="polite">
    <div class="activity" aria-hidden="true">
      <div class="waveform">
        <i></i><i></i><i></i><i></i><i></i>
      </div>
      <div class="spinner"></div>
      <div class="pulse"><i></i><i></i><i></i></div>
      <div class="error-mark">!</div>
    </div>
    <p class="message">Listening</p>
    <button class="stop" type="button" aria-label="Stop dictating">Stop</button>
  </section>
`;

const shell = root.querySelector<HTMLElement>(".overlay-shell")!;
const message = root.querySelector<HTMLElement>(".message")!;
const stop = root.querySelector<HTMLButtonElement>(".stop")!;
const bars = [...root.querySelectorAll<HTMLElement>(".waveform i")];

function render(state: OverlayState): void {
  shell.dataset.phase = state.phase;
  shell.dataset.toggle = String(state.toggleMode);
  message.textContent =
    state.message ??
    ({
      hidden: "",
      initializing: "Opening microphone",
      recording: "Listening",
      transcribing: "Turning speech into text",
      executing: "Answering request",
      error: "Something went wrong",
    } satisfies Record<OverlayState["phase"], string>)[state.phase];
}

function setLevel(level: number): void {
  const clamped = Math.max(0, Math.min(1, level));
  const weights = [0.58, 0.82, 1, 0.78, 0.52];
  bars.forEach((bar, index) => {
    const height = 5 + clamped * weights[index] * 25;
    bar.style.height = `${height}px`;
  });
}

stop.addEventListener("click", () => {
  stop.disabled = true;
  void commands.stopDictating().finally(() => {
    stop.disabled = false;
  });
});

void onOverlayState(render);
void onOverlayLevel(setLevel);
