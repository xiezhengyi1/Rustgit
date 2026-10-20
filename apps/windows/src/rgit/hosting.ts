import type { LanAddress } from "../platform/types";

export const DEFAULT_PORT = 7878;

/** `host:port` for `rgit serve --listen`: loopback unless other devices are allowed. */
export function listenFor(allowLan: boolean, port: string | number): string {
  return `${allowLan ? "0.0.0.0" : "127.0.0.1"}:${port}`;
}

export function portOf(listen: string | null | undefined): string | null {
  const m = /:(\d{1,5})$/.exec(listen ?? "");
  return m ? m[1]! : null;
}

export function isValidPort(p: string): boolean {
  return /^\d{1,5}$/.test(p) && Number(p) >= 0 && Number(p) <= 65535;
}

export interface JoinAddress {
  url: string;
  /** adapter name for LAN addresses */
  label: string | null;
  clone: string;
}

export const cloneCommand = (url: string) => `rgit clone ${url} my-project`;

/**
 * Addresses to give to other people. `addr` is what the host reported (`0.0.0.0:7878` when it
 * listens on every interface, `127.0.0.1:7878` when it is private to this computer).
 */
export function joinAddresses(addr: string | null | undefined, lan: readonly LanAddress[]): { local: boolean; list: JoinAddress[] } {
  const port = portOf(addr);
  if (!addr || !port) return { local: true, list: [] };
  const host = addr.slice(0, addr.length - port.length - 1);
  const loopback = host === "127.0.0.1" || host === "localhost" || host === "[::1]";
  if (loopback) {
    const url = `tcp://${host}:${port}`;
    return { local: true, list: [{ url, label: null, clone: cloneCommand(url) }] };
  }
  if (host === "0.0.0.0" || host === "[::]") {
    return {
      local: false,
      list: lan.map((a) => {
        const url = `tcp://${a.ip}:${port}`;
        return { url, label: a.name, clone: cloneCommand(url) };
      }),
    };
  }
  const url = `tcp://${host}:${port}`; // bound to one specific interface address
  return { local: false, list: [{ url, label: null, clone: cloneCommand(url) }] };
}

/** Copy text to the clipboard; falls back to a hidden textarea where the async API is unavailable. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      return ok;
    } catch {
      return false;
    }
  }
}
