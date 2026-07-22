import { commands, type PublicSettings } from "../shared/ipc";
import "./settings.css";

export function initSettings(root: HTMLElement): void {
root.innerHTML = `
  <main class="settings-shell">
    <header class="pane-head">
      <h1 class="pane-title">Settings</h1>
    </header>

    <form id="settings-form">
      <section class="setting-section" aria-labelledby="provider-heading">
        <div class="section-heading">
          <span class="section-index">01</span>
          <h2 id="provider-heading">Provider</h2>
          <p>Groq Cloud by default; OpenAI-compatible base URLs also work.</p>
          <div class="key-state" data-configured="false"><span></span><strong>Key needed</strong></div>
        </div>
        <div class="section-body">
          <div class="field">
            <label for="api-key">API key</label>
            <input id="api-key" type="password" autocomplete="off" spellcheck="false" placeholder="Paste a new key to replace the saved key" />
            <p class="helper" id="key-helper">Leaving this blank preserves the protected key.</p>
          </div>
          <div class="field">
            <label for="base-url">Base URL</label>
            <input id="base-url" type="url" spellcheck="false" required />
          </div>
          <div class="actions">
            <button id="validate" type="button">Validate credentials</button>
            <button id="clear-key" class="danger" type="button">Clear saved key</button>
          </div>
        </div>
      </section>

      <section class="setting-section" aria-labelledby="dictation-heading">
        <div class="section-heading">
          <span class="section-index">02</span>
          <h2 id="dictation-heading">Dictation</h2>
          <p>Polished runs a cleanup pass for filler words and punctuation; Fast pastes the raw transcript immediately.</p>
        </div>
        <div class="section-body">
          <div class="setting-row">
            <div class="row-label"><strong>Dictation mode</strong></div>
            <div class="segmented" role="radiogroup" aria-label="Dictation mode">
              <label class="segment">
                <input type="radio" name="dictation-mode" value="polished" />
                <span>Polished</span>
              </label>
              <label class="segment">
                <input type="radio" name="dictation-mode" value="fast" />
                <span>Fast</span>
              </label>
            </div>
          </div>
          <div class="setting-row">
            <div class="row-label"><strong>Microphone</strong><small>Input device used while dictating.</small></div>
            <select id="microphone"><option value="">System default</option></select>
          </div>
          <div class="setting-row">
            <div class="row-label"><strong>Custom vocabulary</strong><small>High-priority spellings — names, jargon, product terms.</small></div>
            <input id="custom-vocabulary" type="text" spellcheck="false" placeholder="e.g. Groq, Tauri, DPAPI" />
          </div>
        </div>
      </section>

      <section class="setting-section" aria-labelledby="hotkeys-heading">
        <div class="section-heading">
          <span class="section-index">03</span>
          <h2 id="hotkeys-heading">Hotkeys &amp; voice commands</h2>
          <p>Voice commands are opt-in. End a phrase with "dispatch" to paste it and press Enter, or "execute" to send it to an LLM and paste back the response instead of the transcription.</p>
        </div>
        <div class="section-body">
          <div class="shortcut-row"><span>Hold to talk</span><kbd id="hold-shortcut">Right Ctrl</kbd></div>
          <div class="shortcut-row"><span>Tap to toggle</span><kbd id="toggle-shortcut">F9</kbd></div>
          <div class="shortcut-row"><span>Cancel processing</span><kbd>Esc</kbd></div>
          <label class="setting-row toggle-row" for="commands-beta">
            <div class="row-label"><strong>Voice commands</strong><small>Beta — opt in to spoken commands.</small></div>
            <input id="commands-beta" type="checkbox" />
            <i class="switch" aria-hidden="true"></i>
          </label>
        </div>
      </section>

      <section class="setting-section" aria-labelledby="general-heading">
        <div class="section-heading">
          <span class="section-index">04</span>
          <h2 id="general-heading">General</h2>
        </div>
        <div class="section-body">
          <label class="setting-row toggle-row" for="instruction-guard">
            <div class="row-label"><strong>Instruction guard</strong><small>Blocks the transcript from being executed as an instruction during cleanup.</small></div>
            <input id="instruction-guard" type="checkbox" checked />
            <i class="switch" aria-hidden="true"></i>
          </label>
          <label class="setting-row toggle-row" for="launch-login">
            <div class="row-label"><strong>Launch at login</strong></div>
            <input id="launch-login" type="checkbox" checked />
            <i class="switch" aria-hidden="true"></i>
          </label>
          <label class="setting-row toggle-row" for="sound-cues">
            <div class="row-label"><strong>Sound cues</strong></div>
            <input id="sound-cues" type="checkbox" checked />
            <i class="switch" aria-hidden="true"></i>
          </label>
        </div>
      </section>

      <footer class="save-bar">
        <p id="status" role="status">Loading settings...</p>
        <button id="save" class="primary" type="submit">Save settings</button>
      </footer>
    </form>
  </main>
`;

const form = root.querySelector<HTMLFormElement>("#settings-form")!;
const apiKey = root.querySelector<HTMLInputElement>("#api-key")!;
const baseUrl = root.querySelector<HTMLInputElement>("#base-url")!;
const microphone = root.querySelector<HTMLSelectElement>("#microphone")!;
const dictationModes = Array.from(
  root.querySelectorAll<HTMLInputElement>('input[name="dictation-mode"]'),
);
const status = root.querySelector<HTMLElement>("#status")!;
const save = root.querySelector<HTMLButtonElement>("#save")!;
const validate = root.querySelector<HTMLButtonElement>("#validate")!;
const clearKey = root.querySelector<HTMLButtonElement>("#clear-key")!;
const keyState = root.querySelector<HTMLElement>(".key-state")!;
const commandsBeta = root.querySelector<HTMLInputElement>("#commands-beta")!;

let current: PublicSettings | undefined;

function setStatus(message: string, kind: "neutral" | "success" | "error" = "neutral"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function render(settings: PublicSettings): void {
  current = settings;
  baseUrl.value = settings.baseUrl;
  apiKey.value = "";
  keyState.dataset.configured = String(settings.apiKeyConfigured);
  keyState.querySelector("strong")!.textContent = settings.apiKeyConfigured ? "Key protected" : "Key needed";
  clearKey.disabled = !settings.apiKeyConfigured;
  const selectedMode = dictationModes.find((input) => input.value === settings.dictationMode);
  if (selectedMode) selectedMode.checked = true;

  microphone.replaceChildren(new Option("System default", ""));
  settings.micDevices.forEach((device) => microphone.add(new Option(device, device)));
  microphone.value = settings.micDevice ?? "";
  root.querySelector("#hold-shortcut")!.textContent = settings.holdShortcut;
  root.querySelector("#toggle-shortcut")!.textContent = settings.toggleShortcut;
  commandsBeta.checked = settings.commandsBetaEnabled;
}

function selectedDictationMode(): "fast" | "polished" {
  return dictationModes.find((input) => input.checked)?.value === "fast"
    ? "fast"
    : "polished";
}

async function load(): Promise<void> {
  try {
    render(await commands.getSettings());
    setStatus("Settings are ready.");
  } catch (error) {
    setStatus(String(error), "error");
  }
}

validate.addEventListener("click", async () => {
  validate.disabled = true;
  validate.textContent = "Validating...";
  setStatus("Checking the provider...");
  try {
    await commands.validateCredentials(apiKey.value, baseUrl.value);
    setStatus("Credentials are valid.", "success");
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    validate.disabled = false;
    validate.textContent = "Validate credentials";
  }
});

clearKey.addEventListener("click", async () => {
  if (!current || !window.confirm("Remove the protected API key from this PC?")) return;
  clearKey.disabled = true;
  try {
    render(
      await commands.saveSettings({
        clearApiKey: true,
        baseUrl: baseUrl.value,
        dictationMode: selectedDictationMode(),
        micDevice: microphone.value || undefined,
        commandsBetaEnabled: commandsBeta.checked,
      }),
    );
    setStatus("Saved API key removed.", "success");
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    clearKey.disabled = false;
  }
});

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  save.disabled = true;
  save.textContent = "Saving...";
  try {
    render(
      await commands.saveSettings({
        apiKey: apiKey.value || undefined,
        clearApiKey: false,
        baseUrl: baseUrl.value,
        dictationMode: selectedDictationMode(),
        micDevice: microphone.value || undefined,
        commandsBetaEnabled: commandsBeta.checked,
      }),
    );
    setStatus("Dictation settings saved.", "success");
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    save.disabled = false;
    save.textContent = "Save dictation settings";
  }
});

void load();
}
