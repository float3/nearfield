// Data access. Inside the Android app every request goes through Rust: it
// sidesteps CORS entirely (arxiv.org sends no permissive headers), and it lets
// the Rust side cache each digest for offline reading on the train. In a plain
// browser we fall back to fetch() against the published data directory.

import type { Digest, IndexEntry, Paper } from "./types";

// Relative on purpose: the app is served from a subpath on Pages
// (/nearfield/), so an absolute "/data" would resolve to the wrong host root.
// This is only used by the browser build — inside the Android app every
// request goes through Rust instead.
const DATA_BASE: string =
  (import.meta.env.VITE_DATA_BASE as string | undefined) ?? "./data";

interface TauriInvoke {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

function tauri(): TauriInvoke | null {
  const w = window as unknown as { __TAURI_INTERNALS__?: unknown };
  if (w.__TAURI_INTERNALS__ == null) return null;
  // Imported lazily so the browser build never pulls the Tauri API in.
  return {
    invoke: async <T>(cmd: string, args?: Record<string, unknown>) => {
      const mod = await import("@tauri-apps/api/core");
      return mod.invoke<T>(cmd, args);
    },
  };
}

async function viaHttp<T>(path: string): Promise<T> {
  const res = await fetch(`${DATA_BASE}/${path}`, { cache: "no-cache" });
  if (!res.ok) throw new Error(`${res.status} fetching ${path}`);
  return (await res.json()) as T;
}

export async function loadIndex(): Promise<IndexEntry[]> {
  const t = tauri();
  if (t) return t.invoke<IndexEntry[]>("load_index");
  return viaHttp<IndexEntry[]>("index.json");
}

export async function loadDigest(date: string): Promise<Digest> {
  const t = tauri();
  if (t) return t.invoke<Digest>("load_digest", { date });
  return viaHttp<Digest>(`${date}.json`);
}

/**
 * Fetch one paper's introduction and emails on demand — for any paper outside
 * the nightly prefetch window. Only possible inside the app, since the browser
 * cannot read arxiv.org cross-origin.
 */
export async function fetchPaperDetail(id: string): Promise<Paper> {
  const t = tauri();
  if (!t) {
    throw new Error(
      "On-demand fetching needs the app — a browser cannot read arxiv.org cross-origin.",
    );
  }
  return t.invoke<Paper>("fetch_paper", { id });
}

/** Hand a URL to whichever app the phone uses for it (your PDF reader). */
export async function openExternal(url: string): Promise<void> {
  const t = tauri();
  if (!t) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const mod = await import("@tauri-apps/plugin-opener");
  await mod.openUrl(url);
}

export function canFetchOnDemand(): boolean {
  return tauri() !== null;
}

export interface AppInfo {
  version: string;
  channel: string;
}

export interface UpdateInfo {
  version: string;
  notes: string;
  url: string;
  pub_date: string;
}

export async function appInfo(): Promise<AppInfo | null> {
  const t = tauri();
  if (!t) return null;
  return t.invoke<AppInfo>("app_info");
}

/**
 * Check this build's release channel for something newer. `null` means up to
 * date — or that we are in a browser, where there is nothing to update.
 */
export async function checkUpdate(): Promise<UpdateInfo | null> {
  const t = tauri();
  if (!t) return null;
  return (await t.invoke<UpdateInfo | null>("check_update")) ?? null;
}
