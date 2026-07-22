import { commands, onOverlayLevel, onOverlayState, type OverlayState } from "../shared/ipc";
import "./overlay.css";

const root = document.getElementById("overlay-root")!;
root.innerHTML = `
  <div class="overlay-shell" data-phase="hidden">
    <button class="indicator" type="button" data-phase="hidden" aria-live="polite" aria-label="Murmur">
      <span class="waveform" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i>
      </span>
      <span class="spinner" aria-hidden="true"></span>
      <span class="error-mark" aria-hidden="true">!</span>
    </button>
  </div>
`;

const shell = root.querySelector<HTMLElement>(".overlay-shell")!;
const indicator = root.querySelector<HTMLButtonElement>(".indicator")!;
const bars = [...root.querySelectorAll<HTMLElement>(".waveform i")];

const labels: Record<OverlayState["phase"], string> = {
  hidden: "Murmur",
  initializing: "Opening microphone",
  recording: "Listening",
  transcribing: "Transcribing",
  executing: "Answering request",
  error: "Something went wrong",
};

function render(state: OverlayState): void {
  shell.dataset.phase = state.phase;
  indicator.dataset.phase = state.phase;
  indicator.dataset.toggle = String(state.toggleMode);
  indicator.setAttribute("aria-label", state.message ?? labels[state.phase]);
}

function setLevel(level: number): void {
  const clamped = Math.max(0, Math.min(1, level));
  const weights = [0.58, 0.82, 1, 0.78, 0.52];
  bars.forEach((bar, index) => {
    const height = 4 + clamped * weights[index] * 18;
    bar.style.height = `${height}px`;
  });
}

// The window is click-through except during toggle-mode recording, so a click
// here can only mean "stop dictating" from the capsule itself.
indicator.addEventListener("click", () => {
  if (indicator.dataset.phase !== "recording") return;
  indicator.disabled = true;
  void commands.stopDictating().finally(() => {
    indicator.disabled = false;
  });
});

void onOverlayState(render);
void onOverlayLevel(setLevel);
