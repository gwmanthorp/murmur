import {
  commands,
  onHistoryChanged,
  type HistoryEntry,
} from "../shared/ipc";
import "./history.css";

const root = document.getElementById("history-root")!;
root.innerHTML = `
  <main class="history-shell">
    <header class="history-header">
      <div>
        <p class="eyebrow">MURMUR / LOCAL HISTORY</p>
        <h1>Recent dictations</h1>
        <p class="intro">Your latest 20 messages stay on this PC, protected for your Windows account.</p>
      </div>
      <button id="clear-history" class="danger" type="button" disabled>Clear all</button>
    </header>

    <p id="history-status" class="status" role="status">Loading recent dictations...</p>
    <ol id="history-list" class="history-list" aria-label="Recent dictations"></ol>
    <section id="empty-history" class="empty-state" hidden>
      <p class="empty-index">00</p>
      <div>
        <h2>No dictations saved yet</h2>
        <p>Finish a dictation and it will appear here—even when there was nowhere to paste it.</p>
      </div>
    </section>
  </main>
`;

const list = document.querySelector<HTMLOListElement>("#history-list")!;
const empty = document.querySelector<HTMLElement>("#empty-history")!;
const status = document.querySelector<HTMLElement>("#history-status")!;
const clear = document.querySelector<HTMLButtonElement>("#clear-history")!;

let entries: HistoryEntry[] = [];
let loadGeneration = 0;

const dateFormatter = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  day: "numeric",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
});

function setStatus(message: string, kind: "neutral" | "success" | "error" = "neutral"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function makeButton(label: string, action: "copy" | "delete", id: number): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = label;
  button.dataset.action = action;
  button.dataset.id = String(id);
  if (action === "delete") button.className = "danger row-action";
  else button.className = "row-action";
  return button;
}

function render(nextEntries: HistoryEntry[]): void {
  entries = nextEntries;
  list.replaceChildren();
  empty.hidden = entries.length !== 0;
  list.hidden = entries.length === 0;
  clear.disabled = entries.length === 0;

  entries.forEach((entry, index) => {
    const item = document.createElement("li");
    item.className = "history-entry";

    const rail = document.createElement("div");
    rail.className = "entry-rail";
    const ordinal = document.createElement("span");
    ordinal.className = "entry-index";
    ordinal.textContent = String(entries.length - index).padStart(2, "0");
    const time = document.createElement("time");
    time.dateTime = new Date(entry.createdAtMs).toISOString();
    time.textContent = dateFormatter.format(entry.createdAtMs);
    rail.append(ordinal, time);

    const content = document.createElement("div");
    content.className = "entry-content";
    const text = document.createElement("p");
    text.className = "entry-text";
    text.textContent = entry.text;
    const actions = document.createElement("div");
    actions.className = "entry-actions";
    actions.append(
      makeButton("Copy message", "copy", entry.id),
      makeButton("Delete", "delete", entry.id),
    );
    content.append(text, actions);
    item.append(rail, content);
    list.append(item);
  });
}

async function loadHistory(): Promise<void> {
  const generation = ++loadGeneration;
  try {
    const nextEntries = await commands.getHistory();
    if (generation !== loadGeneration) return;
    render(nextEntries);
    setStatus(
      nextEntries.length === 0
        ? "History is empty."
        : `${nextEntries.length} recent dictation${nextEntries.length === 1 ? "" : "s"}.`,
    );
  } catch (error) {
    if (generation !== loadGeneration) return;
    render([]);
    setStatus(`${String(error)} Close and reopen History to retry.`, "error");
  }
}

list.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-action]");
  if (!button) return;
  const id = Number(button.dataset.id);
  const entry = entries.find((candidate) => candidate.id === id);
  if (!entry) return;

  button.disabled = true;
  if (button.dataset.action === "copy") {
    try {
      await commands.copyHistoryEntry(id);
      button.textContent = "Copied";
      setStatus("Message copied to the clipboard.", "success");
      window.setTimeout(() => {
        button.textContent = "Copy message";
        button.disabled = false;
      }, 1400);
    } catch (error) {
      button.disabled = false;
      setStatus(String(error), "error");
    }
    return;
  }

  button.disabled = false;
  if (!window.confirm("Delete this dictation from History? This cannot be undone.")) return;
  button.disabled = true;
  try {
    await commands.deleteHistoryEntry(id);
    setStatus("Dictation deleted.", "success");
  } catch (error) {
    button.disabled = false;
    setStatus(String(error), "error");
  }
});

clear.addEventListener("click", async () => {
  if (!window.confirm("Clear all dictation History? This cannot be undone.")) return;
  clear.disabled = true;
  clear.textContent = "Clearing...";
  try {
    await commands.clearHistory();
    setStatus("History cleared.", "success");
  } catch (error) {
    clear.disabled = false;
    setStatus(String(error), "error");
  } finally {
    clear.textContent = "Clear all";
  }
});

void onHistoryChanged(() => void loadHistory());
window.addEventListener("focus", () => void loadHistory());
void loadHistory();
