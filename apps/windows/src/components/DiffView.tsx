import { useI18n } from "../i18n";
import type { FileDiff } from "../rgit/diff";

export function DiffView({ file }: { file: FileDiff | undefined }) {
  const { t } = useI18n();
  if (!file) return <p className="empty">{t("changes.noDiff")}</p>;
  if (file.binary) return <p className="empty">{t("changes.binary")}</p>;
  if (file.lines.length === 0) return <p className="empty">{t("changes.noDiff")}</p>;
  return (
    <div className="diff" role="region" aria-label={`${t("changes.diffTitle")}: ${file.path}`} tabIndex={0}>
      <div className="diff-stats">
        <span className="add-text">{t("changes.added", { n: file.added })}</span>{" "}
        <span className="del-text">{t("changes.removed", { n: file.removed })}</span>
      </div>
      <table>
        <tbody>
          {file.lines.map((l, i) => (
            <tr key={i} className={`diff-${l.kind}`}>
              <td className="ln" aria-hidden="true">{l.oldNo ?? ""}</td>
              <td className="ln" aria-hidden="true">{l.newNo ?? ""}</td>
              <td className="sign" aria-hidden="true">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : ""}</td>
              <td className="code">
                {l.kind === "add" ? <span className="sr-only">{t("changes.added", { n: 1 })} </span> : null}
                {l.kind === "del" ? <span className="sr-only">{t("changes.removed", { n: 1 })} </span> : null}
                {l.text}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
