// Typed wrappers around Tauri IPC. All event names and payload shapes shared
// between windows live here so the Rust side (events.rs) has a single mirror.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type OverlayPhase =
  | "hidden"
  | "initializing"
  | "recording"
  | "transcribing"
  | "executing"
  | "error";

export interface OverlayState {
  phase: OverlayPhase;
  toggleMode: boolean;
  message?: string;
}

export interface PublicSettings {
  apiKeyConfigured: boolean;
  baseUrl: string;
  dictationMode: DictationMode;
  micDevice?: string;
  micDevices: string[];
  customVocabulary: string;
  holdShortcut: string;
  toggleShortcut: string;
  preserveClipboard: boolean;
  commandsBetaEnabled: boolean;
}

export interface SaveSettingsInput {
  apiKey?: string;
  clearApiKey: boolean;
  baseUrl: string;
  dictationMode: DictationMode;
  micDevice?: string;
  customVocabulary: string;
  commandsBetaEnabled: boolean;
}

export type DictationMode = "fast" | "polished";

export interface HistoryEntry {
  id: number;
  createdAtMs: number;
  text: string;
}

export function onOverlayState(
  cb: (s: OverlayState) => void,
): Promise<UnlistenFn> {
  return listen<OverlayState>("overlay://state", (e) => cb(e.payload));
}

export function onOverlayLevel(cb: (level: number) => void): Promise<UnlistenFn> {
  return listen<number>("overlay://level", (e) => cb(e.payload));
}

export function onHistoryChanged(cb: () => void): Promise<UnlistenFn> {
  return listen("history://changed", cb);
}

export function onNavigate(
  cb: (pane: "history" | "settings") => void,
): Promise<UnlistenFn> {
  return listen<"history" | "settings">("nav://goto", (e) => cb(e.payload));
}

export const commands = {
  ping: () => invoke<string>("ping"),
  getSettings: () => invoke<PublicSettings>("get_settings"),
  saveSettings: (input: SaveSettingsInput) =>
    invoke<PublicSettings>("save_settings", { input }),
  validateCredentials: (apiKey: string, baseUrl: string) =>
    invoke<void>("validate_credentials", { apiKey, baseUrl }),
  stopDictating: () => invoke<void>("stop_dictating"),
  pasteAgain: () => invoke<void>("paste_again"),
  getHistory: () => invoke<HistoryEntry[]>("get_history"),
  copyHistoryEntry: (id: number) => invoke<void>("copy_history_entry", { id }),
  deleteHistoryEntry: (id: number) => invoke<void>("delete_history_entry", { id }),
  clearHistory: () => invoke<void>("clear_history"),
};
