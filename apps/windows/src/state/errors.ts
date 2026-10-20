import { RgitCancelled, RgitError } from "../rgit/errors";
import { en } from "../i18n/en";
import type { I18n, MessageKey } from "../i18n";

function hasKey(k: string): k is MessageKey {
  return k in en;
}

/** Localized, actionable text for any error thrown by the client or the platform. */
export function errorText(i18n: Pick<I18n, "t">, e: unknown): string {
  if (e instanceof RgitCancelled) return i18n.t("common.cancel");
  if (e instanceof RgitError) {
    const key = `error.${e.code}`;
    if (!hasKey(key)) return e.message;
    const shown = e.paths.slice(0, 5).join(", ") + (e.paths.length > 5 ? ` (+${e.paths.length - 5})` : "");
    return i18n.t(key, { message: e.message, paths: shown });
  }
  return e instanceof Error ? e.message : String(e);
}
