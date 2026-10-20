// A project with ~30,000 changed files (the size a real user hit on macOS): the changes list must
// stay responsive, commit must never put paths on the command line, and no unrelated activity
// may leave Commit disabled. jsdom has no layout engine, so timings here measure React and our
// own logic, not real painting in WebView2; they are a regression guard, not a promise.
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { RunHandle, Runner } from "../src/rgit/runner";
import { renderApp } from "./helpers";

const OPTS = { timeout: 20000 };
const N = 30_000;

function bigStatus(): string {
  const dir = mkdtempSync(join(tmpdir(), "rgit-big-"));
  const changes = Array.from({ length: N }, (_, i) => ({
    path: `packages/pkg${i % 400}/src/generated/file_${i}.ts`,
    kind: i % 3 === 0 ? "added" : "modified",
  }));
  const file = join(dir, "status.big.json");
  writeFileSync(
    file,
    JSON.stringify({
      ok: true,
      data: { branch: "main", head: "a".repeat(64), changes, skipped: [], merge: null, conflicts: [], ahead: 0, behind: 0 },
    }) + "\n",
  );
  return file;
}
const big = bigStatus();
const BIG = [{ argv: ["status"], file: big }];
const changeRows = () => within(screen.getByRole("list", { name: "Changed files" })).getAllByRole("listitem");

describe("30,000 changed files", () => {
  it("renders a window of rows, selects/unselects in O(1), and commits via stdin", async () => {
    const t0 = performance.now();
    const h = renderApp({ overrides: BIG });
    await screen.findByText(`${N} of ${N}`, { exact: false }, OPTS);
    const firstPaint = performance.now() - t0;

    const rows = changeRows();
    expect(rows.length).toBeLessThan(100); // virtualized, not 30,000 DOM rows
    expect(document.querySelectorAll("input[type=checkbox]").length).toBeLessThan(120);
    expect(screen.getByRole("list", { name: "Changed files" })).toBeInTheDocument();
    expect(rows[0]).toHaveAttribute("aria-setsize", String(N));
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeDisabled(); // no message yet

    // untick one file: header count and button label update without touching the other 29,999
    const box = within(rows[3]!).getByRole("checkbox");
    const t1 = performance.now();
    await h.user.click(box);
    const toggleMs = performance.now() - t1;
    expect(await screen.findByText(`${N - 1} of ${N}`, { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeInTheDocument();

    await h.user.type(screen.getByLabelText("Commit message"), "big commit");
    const commitStart = performance.now();
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "commit")).toBe(true), OPTS);
    const call = h.calls().find((c) => c.argv[0] === "commit")!;
    expect(call.argv).toEqual(["commit", "-m", "big commit", "--except-from", "-"]); // 1 path left out, not 29,999 listed
    expect(call.stdinLines).toBe(1);
    expect(call.argv.join(" ").length).toBeLessThan(200);
    console.log(
      `30k changes: first paint ${firstPaint.toFixed(0)} ms, DOM rows ${rows.length}, one toggle ${toggleMs.toFixed(0)} ms, commit round trip ${(performance.now() - commitStart).toFixed(0)} ms`,
    );
    expect(firstPaint).toBeLessThan(15000);
    expect(toggleMs).toBeLessThan(2000);
  }, 60000);

  it("select-all / clear are single operations and a small selection is sent as plain arguments", async () => {
    const h = renderApp({ overrides: BIG });
    await screen.findByText(`${N} of ${N}`, { exact: false }, OPTS);
    const header = screen.getByRole("checkbox", { name: /Select all/ });
    const t0 = performance.now();
    await h.user.click(header);
    expect(await screen.findByText(`0 of ${N}`, { exact: false })).toBeInTheDocument();
    const clearMs = performance.now() - t0;
    const rows = changeRows();
    await h.user.click(within(rows[0]!).getByRole("checkbox"));
    await h.user.click(within(rows[1]!).getByRole("checkbox"));
    await h.user.type(screen.getByLabelText("Commit message"), "two files");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "commit")).toBe(true), OPTS);
    const call = h.calls().find((c) => c.argv[0] === "commit")!;
    // two short paths are fine on the command line; the stdin route for long lists is covered by the 30k tests above/below
    expect(call.argv.slice(3)).toEqual(["packages/pkg0/src/generated/file_0.ts", "packages/pkg1/src/generated/file_1.ts"]);
    expect(call.stdinLines).toBeUndefined();
    console.log(`30k changes: clear-all ${clearMs.toFixed(0)} ms`);
  }, 60000);

  it("does not fetch a 30k-file combined diff on its own, and keyboard navigation reaches the last row", async () => {
    const h = renderApp({ overrides: BIG });
    await screen.findByText(`${N} of ${N}`, { exact: false }, OPTS);
    expect(h.calls().some((c) => c.argv[0] === "diff")).toBe(false);
    expect(screen.getByText(/Differences are not loaded automatically/)).toBeInTheDocument();
    const first = within(changeRows()[0]!).getByRole("button");
    first.focus();
    await h.user.keyboard("{End}");
    const last = await screen.findByText("packages/pkg399/src/generated/file_29999.ts", {}, OPTS);
    expect(last.closest("[role=listitem]")).toHaveAttribute("aria-posinset", String(N));
    await waitFor(() => expect(document.activeElement).toBe(last.closest("[role=listitem]")!.querySelector("[data-row]")));
    expect(changeRows().length).toBeLessThan(100);
  }, 60000);
});

describe("combined submission and activity", () => {
  it("cancelling the send preserves the commit and releases the submission lock even if the child never exits", async () => {
    const stuck = (inner: Runner): Runner => ({
      spawn(req): RunHandle {
        if (req.args[0] !== "push") return inner.spawn(req);
        return { done: new Promise(() => {}), cancel() {} };
      },
    });
    const h = renderApp({ wrapRunner: stuck });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "saved before cancel");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await screen.findByRole("progressbar", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Retry sharing" })).toBeEnabled(), OPTS);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByLabelText("Commit message")).toHaveValue("");
    expect(h.calls().filter((c) => c.argv[0] === "commit")).toHaveLength(1);
    expect(await screen.findByRole("alert", {}, OPTS)).toHaveTextContent("Saved locally, but not shared");
  }, 40000);

  it("a combined submission waits for an active push; cancellation makes it usable again", async () => {
    const h = renderApp({ overrides: [{ argv: ["push"], file: "push.hang.ndjson", hang: true }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("button", { name: "Push" })).toBeEnabled(), OPTS);
    await h.user.click(screen.getByRole("button", { name: "Push" }));
    await screen.findByRole("progressbar", {}, OPTS);
    expect(screen.getByRole("button", { name: "Push" })).toBeDisabled(); // no second push
    await h.user.type(screen.getByLabelText("Commit message"), "while pushing");
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeDisabled();
    await h.user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("progressbar")).toBeNull(), OPTS);
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeEnabled();
  }, 40000);

  it("Cancel returns the UI to idle at once even if the process never reports its exit", async () => {
    // A runner whose cancelled push never resolves: the worst case for a stuck-activity bug.
    const stuck = (inner: Runner): Runner => ({
      spawn(req): RunHandle {
        if (req.args[0] !== "push") return inner.spawn(req);
        req.onStdoutLine?.(JSON.stringify({ event: "progress", phase: "uploading", done: 1, total: 9 }));
        return { done: new Promise(() => {}), cancel() { /* process termination is asynchronous and may be slow */ } };
      },
    });
    const h = renderApp({ wrapRunner: stuck });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("button", { name: "Push" })).toBeEnabled(), OPTS);
    await h.user.click(screen.getByRole("button", { name: "Push" }));
    await screen.findByRole("progressbar", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: "Cancel" }));
    // idle immediately: no progress, Push usable again, Commit usable
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByRole("button", { name: "Push" })).toBeEnabled();
    await h.user.type(screen.getByLabelText("Commit message"), "after cancel");
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeEnabled();
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "commit")).toBe(true), OPTS);
    expect(screen.queryByRole("alert")).toBeNull();
  }, 40000);

  it("a failing call releases the busy lock", async () => {
    const h = renderApp({ overrides: [{ argv: ["commit", "-m", "*"], file: "commit.nothing.json", exit: 1 }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "x");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await screen.findByRole("alert", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("button", { name: "Commit and share" })).toBeEnabled(), OPTS);
    act(() => undefined);
  }, 40000);
});
