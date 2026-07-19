import { listen } from "@tauri-apps/api/event";
import { commands } from "../shared/ipc";

const root = document.getElementById("settings-root")!;
root.innerHTML = `
  <div style="padding: 24px; max-width: 640px; margin: 0 auto;">
    <h1>Murmur Settings</h1>
    <p id="status" style="color: var(--text-dim)">Connecting to backend…</p>
    <h2>Shortcut debug</h2>
    <p style="color: var(--text-dim)">Hold Right Ctrl to talk · tap F9 to toggle · Esc cancels</p>
    <pre id="shortcut-log" style="background: var(--bg-raised); border-radius: 8px; padding: 12px; min-height: 160px; font-size: 12px;"></pre>
  </div>
`;

const log = document.getElementById("shortcut-log")!;
listen<string>("debug://shortcut", (e) => {
  log.textContent = `${new Date().toLocaleTimeString()}  ${e.payload}\n${log.textContent}`.slice(0, 4000);
});

commands
  .ping()
  .then((v) => {
    document.getElementById("status")!.textContent = `Backend OK (${v})`;
  })
  .catch((e) => {
    document.getElementById("status")!.textContent = `Backend error: ${e}`;
  });
