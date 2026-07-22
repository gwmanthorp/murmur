import {
  commands,
  onHistoryChanged,
  type HistoryEntry,
} from "../shared/ipc";
import "./history.css";

export function initHistory(root: HTMLElement): void {
  root.innerHTML = `
  <main class="history-shell">
    <header class="pane-head">
      <h1 class="pane-title">History</h1>
      <input id="history-search" class="history-search" type="search" placeholder="Search transcripts" aria-label="Search transcripts" />
    </header>
    <div class="history-body">
      <div class="history-master">
        <ol id="history-list" class="history-list" aria-label="Recent dictations"></ol>
        <button id="clear-history" class="link-danger" type="button" hidden>Clear all history</button>
      </div>
      <section id="history-detail" class="history-detail" aria-live="polite"></section>
    </div>
  </main>
`;

  const search = root.querySelector<HTMLInputElement>("#history-search")!;
  const list = root.querySelector<HTMLOListElement>("#history-list")!;
  const detail = root.querySelector<HTMLElement>("#history-detail")!;
  const clear = root.querySelector<HTMLButtonElement>("#clear-history")!;

  let entries: HistoryEntry[] = [];
  let selectedId: number | null = null;
  let query = "";
  let loadGeneration = 0;

  const timeFormatter = new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
  const weekdayFormatter = new Intl.DateTimeFormat(undefined, { weekday: "short" });
  const dateFormatter = new Intl.DateTimeFormat(undefined, {
    day: "numeric",
    month: "short",
  });

  function startOfDay(ms: number): number {
    const date = new Date(ms);
    date.setHours(0, 0, 0, 0);
    return date.getTime();
  }

  function relativeLabel(ms: number): string {
    const days = Math.round((startOfDay(Date.now()) - startOfDay(ms)) / 86_400_000);
    const time = timeFormatter.format(ms);
    if (days <= 0) return `Today, ${time}`;
    if (days === 1) return `Yesterday, ${time}`;
    if (days < 7) return `${weekdayFormatter.format(ms)}, ${time}`;
    return `${dateFormatter.format(ms)}, ${time}`;
  }

  function detailStamp(ms: number): string {
    return relativeLabel(ms).toUpperCase();
  }

  function deriveTitle(text: string): string {
    const firstLine = text.trim().split(/\r?\n/)[0]?.trim() ?? "";
    const sentence = firstLine.split(/(?<=[.!?])\s/)[0] ?? firstLine;
    const words = sentence.split(/\s+/).slice(0, 6).join(" ");
    const title = words.length > 48 ? `${words.slice(0, 47).trimEnd()}…` : words;
    return title || "Untitled dictation";
  }

  function visibleEntries(): HistoryEntry[] {
    if (!query) return entries;
    const needle = query.toLowerCase();
    return entries.filter(
      (entry) =>
        entry.text.toLowerCase().includes(needle) ||
        deriveTitle(entry.text).toLowerCase().includes(needle),
    );
  }

  function renderDetail(): void {
    const entry = entries.find((candidate) => candidate.id === selectedId);
    if (!entry) {
      detail.dataset.empty = "true";
      detail.innerHTML = `<p class="detail-empty">${
        entries.length === 0
          ? "Finish a dictation and it will appear here."
          : "Select a transcript to read it."
      }</p>`;
      return;
    }
    detail.dataset.empty = "false";
    detail.replaceChildren();

    const bar = document.createElement("div");
    bar.className = "detail-bar";
    const stamp = document.createElement("span");
    stamp.className = "detail-stamp";
    stamp.textContent = detailStamp(entry.createdAtMs);
    const actions = document.createElement("div");
    actions.className = "detail-actions";
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "link";
    copy.dataset.action = "copy";
    copy.textContent = "Copy";
    const del = document.createElement("button");
    del.type = "button";
    del.className = "link link-danger";
    del.dataset.action = "delete";
    del.textContent = "Delete";
    actions.append(copy, del);
    bar.append(stamp, actions);

    const card = document.createElement("div");
    card.className = "detail-card";
    const body = document.createElement("p");
    body.className = "detail-text";
    body.textContent = entry.text;
    card.append(body);

    detail.append(bar, card);
  }

  function renderList(): void {
    const shown = visibleEntries();
    list.replaceChildren();
    clear.hidden = entries.length === 0;

    if (shown.length === 0) {
      const empty = document.createElement("li");
      empty.className = "history-empty";
      empty.textContent = entries.length === 0 ? "No dictations yet." : "No matches.";
      list.append(empty);
      return;
    }

    shown.forEach((entry) => {
      const item = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      button.className = "history-item";
      button.dataset.id = String(entry.id);
      button.classList.toggle("active", entry.id === selectedId);

      const title = document.createElement("span");
      title.className = "item-title";
      title.textContent = deriveTitle(entry.text);
      const time = document.createElement("time");
      time.className = "item-time";
      time.dateTime = new Date(entry.createdAtMs).toISOString();
      time.textContent = relativeLabel(entry.createdAtMs);

      button.append(title, time);
      item.append(button);
      list.append(item);
    });
  }

  function render(): void {
    // Keep a valid selection: default to the newest visible entry.
    const shown = visibleEntries();
    if (!shown.some((entry) => entry.id === selectedId)) {
      selectedId = shown[0]?.id ?? null;
    }
    renderList();
    renderDetail();
  }

  async function loadHistory(): Promise<void> {
    const generation = ++loadGeneration;
    try {
      const nextEntries = await commands.getHistory();
      if (generation !== loadGeneration) return;
      entries = nextEntries;
      render();
    } catch (error) {
      if (generation !== loadGeneration) return;
      entries = [];
      selectedId = null;
      renderList();
      detail.dataset.empty = "true";
      detail.innerHTML = `<p class="detail-empty">${String(error)}</p>`;
    }
  }

  search.addEventListener("input", () => {
    query = search.value.trim();
    render();
  });

  list.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>(".history-item");
    if (!button) return;
    selectedId = Number(button.dataset.id);
    render();
  });

  detail.addEventListener("click", async (event) => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-action]");
    if (!button || selectedId === null) return;
    const id = selectedId;

    if (button.dataset.action === "copy") {
      button.disabled = true;
      try {
        await commands.copyHistoryEntry(id);
        button.textContent = "Copied";
        window.setTimeout(() => {
          button.textContent = "Copy";
          button.disabled = false;
        }, 1400);
      } catch {
        button.disabled = false;
      }
      return;
    }

    if (!window.confirm("Delete this dictation from History? This cannot be undone.")) return;
    button.disabled = true;
    try {
      await commands.deleteHistoryEntry(id);
    } catch {
      button.disabled = false;
    }
  });

  clear.addEventListener("click", async () => {
    if (!window.confirm("Clear all dictation History? This cannot be undone.")) return;
    clear.disabled = true;
    try {
      await commands.clearHistory();
    } catch {
      clear.disabled = false;
    }
  });

  void onHistoryChanged(() => void loadHistory());
  window.addEventListener("focus", () => void loadHistory());
  void loadHistory();
}
