import { onNavigate } from "../shared/ipc";
import { initSettings } from "../settings/main";
import { initHistory } from "../history/main";
import "./app.css";

type Pane = "history" | "settings";

const root = document.getElementById("app-root")!;
root.innerHTML = `
  <div class="app-shell">
    <nav class="app-nav" aria-label="Sections">
      <button class="nav-item" type="button" data-pane="history">History</button>
      <button class="nav-item" type="button" data-pane="settings">Settings</button>
    </nav>
    <div class="app-content">
      <div id="history-root" class="pane" data-pane="history"></div>
      <div id="settings-root" class="pane" data-pane="settings"></div>
    </div>
  </div>
`;

const navItems = Array.from(
  root.querySelectorAll<HTMLButtonElement>(".nav-item"),
);
const panes = Array.from(root.querySelectorAll<HTMLElement>(".pane"));

function showPane(pane: Pane): void {
  navItems.forEach((item) => {
    item.classList.toggle("active", item.dataset.pane === pane);
    item.setAttribute("aria-current", item.dataset.pane === pane ? "page" : "false");
  });
  panes.forEach((element) => {
    element.hidden = element.dataset.pane !== pane;
  });
}

navItems.forEach((item) => {
  item.addEventListener("click", () => showPane(item.dataset.pane as Pane));
});

initHistory(document.getElementById("history-root")!);
initSettings(document.getElementById("settings-root")!);

showPane("settings");
void onNavigate((pane) => showPane(pane));
