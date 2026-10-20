import {
  useEffect,
  useId,
  useRef,
  type ButtonHTMLAttributes,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useI18n } from "../i18n";

export function Button({
  variant = "default",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "danger" | "quiet" }) {
  return <button type="button" {...rest} className={`btn btn-${variant} ${rest.className ?? ""}`} />;
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: (props: { id: string; "aria-describedby"?: string }) => ReactNode;
}) {
  const id = useId();
  const hintId = hint ? `${id}-hint` : undefined;
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children({ id, "aria-describedby": hintId })}
      {hint ? (
        <p className="hint" id={hintId}>
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function ProgressBar({ done, total, label }: { done: number; total: number; label: string }) {
  const known = total > 0;
  return (
    <div
      className="progress"
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={known ? total : undefined}
      aria-valuenow={known ? done : undefined}
    >
      <div
        className={`progress-fill ${known ? "" : "indeterminate"}`}
        style={known ? { width: `${Math.min(100, (done / total) * 100)}%` } : undefined}
      />
    </div>
  );
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Modal dialog: traps Tab, closes on Escape, returns focus to the opener. */
export function Dialog({
  title,
  onClose,
  children,
  footer,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const { t } = useI18n();

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>("input, select, textarea") ?? ref.current?.querySelector<HTMLElement>(FOCUSABLE);
    first?.focus();
    return () => opener?.focus?.();
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "Tab" || !ref.current) return;
    const items = Array.from(ref.current.querySelectorAll<HTMLElement>(FOCUSABLE));
    if (items.length === 0) return;
    const first = items[0]!;
    const last = items[items.length - 1]!;
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="backdrop">
      <div ref={ref} className="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId} onKeyDown={onKeyDown}>
        <h2 id={titleId}>{title}</h2>
        <div className="dialog-body">{children}</div>
        <div className="dialog-footer">
          {footer ?? (
            <Button onClick={onClose}>{t("common.close")}</Button>
          )}
        </div>
      </div>
    </div>
  );
}

export function ConfirmDialog({
  title,
  body,
  confirmLabel,
  danger,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  return (
    <Dialog
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel}>{t("common.cancel")}</Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      {typeof body === "string" ? <p>{body}</p> : body}
    </Dialog>
  );
}

/** Arrow-key navigation for a vertical list of focusable rows. */
export function useRovingList() {
  const ref = useRef<HTMLUListElement>(null);
  const onKeyDown = (e: KeyboardEvent<HTMLUListElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
    const rows = Array.from(ref.current?.querySelectorAll<HTMLElement>("[data-row]") ?? []);
    if (rows.length === 0) return;
    const idx = rows.indexOf(document.activeElement as HTMLElement);
    let next = idx;
    if (e.key === "ArrowDown") next = Math.min(rows.length - 1, idx + 1);
    if (e.key === "ArrowUp") next = Math.max(0, idx - 1);
    if (e.key === "Home") next = 0;
    if (e.key === "End") next = rows.length - 1;
    e.preventDefault();
    rows[next]?.focus();
  };
  return { ref, onKeyDown };
}

export function Section({ title, children, actions, id }: { title: string; children: ReactNode; actions?: ReactNode; id?: string }) {
  const hid = useId();
  return (
    <section className="card" aria-labelledby={hid} id={id}>
      <header className="card-head">
        <h2 id={hid}>{title}</h2>
        {actions ? <div className="card-actions">{actions}</div> : null}
      </header>
      {children}
    </section>
  );
}

export function EmptyNote({ children }: { children: ReactNode }) {
  return <p className="empty">{children}</p>;
}
