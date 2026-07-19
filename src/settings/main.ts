import { commands, type PublicSettings } from "../shared/ipc";
import "./settings.css";

const root = document.getElementById("settings-root")!;
root.innerHTML = `
  <main class="settings-shell">
    <header class="masthead">
      <div>
        <p class="eyebrow">MURMUR / CORE DICTATION</p>
        <h1>Speak where the cursor is.</h1>
        <p class="intro">Your audio goes directly to the provider you configure. The API key is protected by Windows and never shown again.</p>
      </div>
      <div class="key-state" data-configured="false"><span></span><strong>Key needed</strong></div>
    </header>

    <form id="settings-form">
      <section class="form-section" aria-labelledby="provider-heading">
        <div class="section-heading">
          <span>01</span>
          <div><h2 id="provider-heading">Provider</h2><p>Groq Cloud by default; OpenAI-compatible base URLs also work.</p></div>
        </div>
        <div class="field-grid">
          <div class="field full">
            <label for="api-key">API key</label>
            <input id="api-key" type="password" autocomplete="off" spellcheck="false" placeholder="Paste a new key to replace the saved key" />
            <p class="helper" id="key-helper">Leaving this blank preserves the protected key.</p>
          </div>
          <div class="field full">
            <label for="base-url">Base URL</label>
            <input id="base-url" type="url" spellcheck="false" required />
          </div>
        </div>
        <div class="actions">
          <button id="validate" type="button">Validate credentials</button>
          <button id="clear-key" class="danger" type="button">Clear saved key</button>
        </div>
      </section>

      <section class="form-section" aria-labelledby="capture-heading">
        <div class="section-heading">
          <span>02</span>
          <div><h2 id="capture-heading">Capture</h2><p>Choose the microphone Murmur opens for each dictation.</p></div>
        </div>
        <div class="field">
          <label for="microphone">Microphone</label>
          <select id="microphone"><option value="">System default</option></select>
        </div>
      </section>

      <section class="shortcut-section" aria-labelledby="shortcuts-heading">
        <div class="section-heading">
          <span>03</span>
          <div><h2 id="shortcuts-heading">Shortcuts</h2><p>Rebinding arrives with the full settings pass.</p></div>
        </div>
        <dl>
          <div><dt>Hold to talk</dt><dd id="hold-shortcut">Right Ctrl</dd></div>
          <div><dt>Tap to toggle</dt><dd id="toggle-shortcut">F9</dd></div>
          <div><dt>Cancel processing</dt><dd>Esc</dd></div>
        </dl>
      </section>

      <footer class="save-bar">
        <p id="status" role="status">Loading settings...</p>
        <button id="save" class="primary" type="submit">Save dictation settings</button>
      </footer>
    </form>
  </main>
`;

const form = document.querySelector<HTMLFormElement>("#settings-form")!;
const apiKey = document.querySelector<HTMLInputElement>("#api-key")!;
const baseUrl = document.querySelector<HTMLInputElement>("#base-url")!;
const microphone = document.querySelector<HTMLSelectElement>("#microphone")!;
const status = document.querySelector<HTMLElement>("#status")!;
const save = document.querySelector<HTMLButtonElement>("#save")!;
const validate = document.querySelector<HTMLButtonElement>("#validate")!;
const clearKey = document.querySelector<HTMLButtonElement>("#clear-key")!;
const keyState = document.querySelector<HTMLElement>(".key-state")!;

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

  microphone.replaceChildren(new Option("System default", ""));
  settings.micDevices.forEach((device) => microphone.add(new Option(device, device)));
  microphone.value = settings.micDevice ?? "";
  document.querySelector("#hold-shortcut")!.textContent = settings.holdShortcut;
  document.querySelector("#toggle-shortcut")!.textContent = settings.toggleShortcut;
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
        micDevice: microphone.value || undefined,
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
        micDevice: microphone.value || undefined,
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
