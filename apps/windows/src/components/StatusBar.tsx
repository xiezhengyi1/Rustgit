import { useI18n, type MessageKey } from "../i18n";
import { baseName, formatBytes } from "../rgit/format";
import type { StatusData } from "../rgit/types";
import type { ServeStatus } from "../platform/types";
import { useApp, type ActiveOp } from "../state/app";
import { Button, ProgressBar } from "./ui";

export interface SyncSummary {
  hasRemote: boolean;
  ahead: number;
  behind: number;
}

/** Derive the three "where is my work" facts from status + configured remotes. */
export function syncSummary(status: StatusData | null, remoteCount: number): SyncSummary {
  return { hasRemote: remoteCount > 0, ahead: status?.ahead ?? 0, behind: status?.behind ?? 0 };
}

export function serveLabel(t: (k: MessageKey, p?: Record<string, string | number>) => string, serve: ServeStatus): string {
  if (serve.running) {
    return serve.addr ? t("status.serverRunning", { addr: serve.addr }) : t("status.serverStarting");
  }
  if (serve.lastError) return t("status.serverError", { error: serve.lastError });
  return t("status.serverStopped");
}

function opLabel(t: ReturnType<typeof useI18n>["t"], op: ActiveOp): string {
  return t(`op.${op.kind}` as MessageKey);
}

export function StatusBar() {
  const { t, intlLocale } = useI18n();
  const { state, actions } = useApp();
  const { project, status, remotes, serve, op, lastSync } = state;
  const s = syncSummary(status, remotes.length);
  const canSync = !!project && s.hasRemote && !state.busy && !op;

  return (
    <div className="statusbar" role="region" aria-label={t("status.region")}>
      <div className="status-item">
        <span className="status-label">{t("status.branch")}</span>
        <span className="badge" data-testid="branch">
          {project ? (status?.branch ?? "…") : t("status.noProject")}
        </span>
        {project ? <span className="muted project-name" title={project}>{baseName(project)}</span> : null}
      </div>

      <div className="status-item" data-testid="sync-summary">
        <span className="status-label">{t("status.sync")}</span>
        {!project ? (
          <span className="muted">–</span>
        ) : !s.hasRemote ? (
          <span>{t("status.noRemote")}</span>
        ) : (
          <span>
            {s.ahead > 0
              ? t(state.neverPushed ? "status.neverPushed" : "status.committedLocal", { n: s.ahead })
              : t("status.receivedByServer")}
            {s.behind > 0 ? ` · ${t("status.toFetch", { n: s.behind })}` : ""}
          </span>
        )}
        {project && s.hasRemote ? <span className="sr-only">{t("status.peerFetch")}</span> : null}
        {project && s.hasRemote ? (
          <span className="info-tip" title={t("status.peerFetch")} aria-hidden="true">
            ⓘ
          </span>
        ) : null}
      </div>

      <div className="status-item" data-testid="server-state">
        <span className="status-label">{t("status.server")}</span>
        <span className={serve.running ? "dot dot-on" : "dot dot-off"} aria-hidden="true" />
        <span title={t("status.sleepNote")}>{serveLabel(t, serve)}</span>
      </div>

      <div className="status-actions">
        {state.scanning ? <span className="muted" role="status">{t("status.scanning")}</span> : null}
        <Button variant="quiet" onClick={() => void actions.refresh()} disabled={!project || !!op || state.scanning}>
          {t("status.refresh")}
        </Button>
        <Button onClick={() => void actions.sync("fetch")} disabled={!canSync}>
          {t("status.fetch")}
        </Button>
        <Button onClick={() => void actions.sync("pull")} disabled={!canSync}>
          {t("status.pull")}
        </Button>
        <Button variant="primary" onClick={() => void actions.sync("push")} disabled={!canSync}>
          {t("status.push")}
        </Button>
      </div>

      <div className="status-progress" aria-live="polite">
        {op ? (
          <>
            <ProgressBar done={op.done} total={op.total} label={opLabel(t, op)} />
            <span>
              {t("status.working", { phase: op.phase ?? opLabel(t, op) })}
              {op.total > 0 ? ` · ${t("status.progress", { done: op.done, total: op.total })}` : ""}
              {op.bytes ? ` · ${formatBytes(op.bytes, intlLocale)}` : ""}
            </span>
            <Button variant="quiet" onClick={actions.cancelOp}>
              {t("status.cancel")}
            </Button>
          </>
        ) : lastSync ? (
          <span className={lastSync.ok ? "muted" : "error-text"}>
            {t("status.lastSync", {
              op: t(`op.${lastSync.kind}` as MessageKey),
              result: lastSync.ok ? "✓" : (lastSync.message ?? "×"),
            })}
          </span>
        ) : null}
      </div>
    </div>
  );
}
