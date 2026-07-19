// Typed wrappers around Tauri IPC. All event names and payload shapes shared
// between windows live here so the Rust side (events.rs) has a single mirror.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type OverlayPhase =
  | "hidden"
  | "initializing"
  | "recording"
  | "transcribing"
  | "error";

export interface OverlayState {
  phase: OverlayPhase;
  toggleMode: boolean;
  message?: string;
}

export function onOverlayState(
  cb: (s: OverlayState) => void,
): Promise<UnlistenFn> {
  return listen<OverlayState>("overlay://state", (e) => cb(e.payload));
}

export function onOverlayLevel(cb: (level: number) => void): Promise<UnlistenFn> {
  return listen<number>("overlay://level", (e) => cb(e.payload));
}

export const commands = {
  ping: () => invoke<string>("ping"),
};
