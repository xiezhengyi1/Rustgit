import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { en } from "../src/i18n/en";
import { zh } from "../src/i18n/zh";
import { resolveLocale, translate } from "../src/i18n";
import { renderApp } from "./helpers";

const OPTS = { timeout: 8000 };

describe("i18n", () => {
  it("English and Chinese define exactly the same keys, none empty", () => {
    expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort());
    for (const [k, v] of Object.entries(zh)) expect(v.trim(), k).not.toBe("");
  });

  it("uses the same placeholders in both languages", () => {
    const ph = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort().join(",");
    for (const k of Object.keys(en) as (keyof typeof en)[]) expect(ph(zh[k]), k).toBe(ph(en[k]));
  });

  it("resolves the locale from the setting or the system language", () => {
    expect(resolveLocale("auto", "zh-CN")).toBe("zh");
    expect(resolveLocale("auto", "en-US")).toBe("en");
    expect(resolveLocale("en", "zh-CN")).toBe("en");
    expect(translate("zh", "status.committedLocal", { n: 3 })).toContain("3");
  });
});

describe("App against the fake rgit", () => {
  it("reopens the last project and shows branch, sync state and changes", async () => {
    renderApp();
    expect(await screen.findByText("docs/readme.md", {}, OPTS)).toBeInTheDocument();
    expect(screen.getByTestId("branch")).toHaveTextContent("main");
    expect(screen.getByRole("heading", { name: "Changes" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("sync-summary")).toHaveTextContent("Committed locally: 2"), OPTS);
    expect(screen.getByTestId("sync-summary")).toHaveTextContent("1 new on the server");
    expect(screen.getByTestId("server-state")).toHaveTextContent("Not hosting");
  });

  it("shows the diff of the selected file", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await waitFor(() => expect(screen.getByText("Edited line")).toBeInTheDocument(), OPTS);
    await h.user.click(screen.getByRole("button", { name: /new_file\.rs/ }));
    expect(await screen.findByText("fn main() {}", {}, OPTS)).toBeInTheDocument();
  });

  it("commits only the selected files", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("checkbox", { name: "Include old/notes.txt in the commit" }));
    await h.user.click(screen.getByRole("checkbox", { name: "Include src/new_file.rs in the commit" }));
    const commit = screen.getByRole("button", { name: "Commit and share" });
    expect(commit).toBeDisabled(); // no message yet
    await h.user.type(screen.getByLabelText("Commit message"), "tidy up");
    await h.user.click(commit);
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "commit")).toBe(true), OPTS);
    const call = h.calls().find((c) => c.argv[0] === "commit")!;
    expect(call.project).toBe("/work/demo");
    expect(call.argv).toEqual(["commit", "-m", "tidy up", "docs/readme.md", "src/lib2.rs", "src/lib.rs"]);
    expect(await screen.findByText("Committed and shared. Other devices can now pull this version.", {}, OPTS)).toBeInTheDocument();
  });

  it("commits everything without paths when all files are selected", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "all");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "commit")).toBe(true), OPTS);
    expect(h.calls().find((c) => c.argv[0] === "commit")!.argv).toEqual(["commit", "-m", "all"]);
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "push")).toBe(true), OPTS);
  });

  it("keeps an offline project as a local commit", async () => {
    const h = renderApp({ overrides: [{ argv: ["remote"], file: "remote.empty.json" }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "offline");
    await h.user.click(screen.getByRole("button", { name: "Commit all changes" }));
    await screen.findByText(/Committed .* on main/, {}, OPTS);
    expect(h.calls().filter((c) => c.argv[0] === "commit")).toHaveLength(1);
    expect(h.calls().some((c) => c.argv[0] === "push")).toBe(false);
  });

  it("does not send anything if committing failed", async () => {
    const h = renderApp({ overrides: [{ argv: ["commit", "-m", "*"], file: "commit.nothing.json", exit: 1 }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "not committed");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    await screen.findByRole("alert", {}, OPTS);
    expect(h.calls().some((c) => c.argv[0] === "push")).toBe(false);
    expect(screen.getByLabelText("Commit message")).toHaveValue("not committed");
  });

  it("keeps a successful commit when sending fails and retries only the send", async () => {
    const h = renderApp({ overrides: [{ argv: ["push"], file: "push.rejected.ndjson", exit: 1 }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "safe local commit");
    await h.user.click(screen.getByRole("button", { name: "Commit and share" }));
    expect(await screen.findByRole("alert", {}, OPTS)).toHaveTextContent("Saved locally, but not shared");
    expect(screen.getByLabelText("Commit message")).toHaveValue("");
    const retry = screen.getByRole("button", { name: "Retry sharing" });
    await waitFor(() => expect(retry).toBeEnabled(), OPTS);
    await h.user.click(retry);
    await waitFor(() => expect(h.calls().filter((c) => c.argv[0] === "push")).toHaveLength(2), OPTS);
    expect(h.calls().filter((c) => c.argv[0] === "commit")).toHaveLength(1);
  });

  it("refuses an engine with another json_interface", async () => {
    renderApp({ overrides: [{ argv: ["version"], file: "version.mismatch.json" }] });
    expect(await screen.findByText("The bundled engine could not be used", {}, OPTS)).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(/interface/i);
  });

  it("offers to create a project in a folder that is not one", async () => {
    const h = renderApp({
      settings: { lastProject: null },
      overrides: [{ argv: ["status"], file: "status.notrepo.json", exit: 1 }],
    });
    await screen.findByRole("heading", { name: "Projects" }, OPTS);
    h.picked.value = "/work/empty";
    await h.user.click(screen.getByRole("button", { name: "Open a folder…" }));
    const dialog = await screen.findByRole("dialog", {}, OPTS);
    expect(dialog).toHaveTextContent("/work/empty is not an rgit project yet");
    await h.user.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "init")).toBe(true), OPTS);
    expect(h.calls().find((c) => c.argv[0] === "init")).toEqual({ project: "/work/empty", argv: ["init", "--branch", "main"] });
  });

  it("switches language to Chinese", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /Settings/ }));
    await h.user.selectOptions(screen.getByLabelText("Language"), "zh");
    await waitFor(() => expect(screen.getByRole("navigation", { name: "主导航" })).toBeInTheDocument(), OPTS);
    expect(document.documentElement.lang).toBe("zh-CN");
    expect(h.settings().language).toBe("zh");
  });

  it("navigates with Ctrl+number and marks the current page", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.keyboard("{Control>}4{/Control}");
    expect(await screen.findByRole("heading", { name: "Sharing & connections" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Sharing/ })).toHaveAttribute("aria-current", "page");
  });

  it("offers authenticated sharing and explains that access management belongs to the host", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /^.?\s?Sharing/ }));
    expect(await screen.findByText("Manage invitations and devices on the computer hosting the project.", {}, OPTS)).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Not available yet" })).toBeNull();
    expect(await screen.findByRole("heading", { name: "Nearby hosts" })).toBeInTheDocument();
    await waitFor(() => expect(h.calls().some(c => c.argv.join(" ") === "device id")).toBe(true), OPTS);
  });

  it("shows sync progress and lets the user cancel a running push", async () => {
    const h = renderApp({ overrides: [{ argv: ["push"], file: "push.hang.ndjson", hang: true }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("button", { name: "Push" })).toBeEnabled(), OPTS);
    await h.user.click(screen.getByRole("button", { name: "Push" }));
    const bar = await screen.findByRole("progressbar", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "1"), OPTS);
    expect(bar).toHaveAttribute("aria-valuemax", "400");
    const cancel = await screen.findByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toBeEnabled());
    await h.user.click(cancel);
    await waitFor(() => expect(screen.queryByRole("progressbar")).toBeNull(), OPTS);
    expect(screen.queryByRole("alert")).toBeNull(); // cancelling is not an error
  });

  it("explains a rejected push in plain words", async () => {
    const h = renderApp({ overrides: [{ argv: ["push"], file: "push.rejected.ndjson", exit: 1 }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await waitFor(() => expect(screen.getByRole("button", { name: "Push" })).toBeEnabled(), OPTS);
    await h.user.click(screen.getByRole("button", { name: "Push" }));
    const alert = await screen.findByRole("alert", {}, OPTS);
    expect(alert).toHaveTextContent("Fetch and merge it, then push again");
  });

  it("shows hosting state from the platform and the sleep warning", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    act(() => h.emitServeState({ running: true, addr: "127.0.0.1:7878", dir: "/srv/p.rgit" }));
    expect(screen.getByTestId("server-state")).toHaveTextContent("Hosting on 127.0.0.1:7878");
    expect(screen.getByText("Hosting on 127.0.0.1:7878")).toHaveAttribute("title", expect.stringContaining("sleeps"));
    act(() => h.emitServeState({ running: false, lastError: "address in use" }));
    expect(screen.getByTestId("server-state")).toHaveTextContent("Hosting stopped: address in use");
  });
});

describe("Hosting join address", () => {
  const lan = [
    { ip: "192.168.1.20", name: "Wi-Fi", private: true },
    { ip: "172.28.80.1", name: "vEthernet (WSL)", private: true },
  ];

  async function openProjects(platform = {}) {
    const h = renderApp({ platform: { lanAddresses: async () => lan, ...platform } });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /Projects/ }));
    await screen.findByRole("heading", { name: "Host a project" }, OPTS);
    return h;
  }

  it("local-only hosting shows only the loopback address and says so", async () => {
    const h = await openProjects();
    act(() => h.emitServeState({ running: true, addr: "127.0.0.1:7878", dir: "C:\\srv\\p.rgit" }));
    const group = await screen.findByRole("group", { name: "Address for other devices" });
    expect(within(group).getByTestId("join-url")).toHaveTextContent("tcp://127.0.0.1:7878");
    expect(group).toHaveTextContent("Only this computer can connect");
    expect(group).not.toHaveTextContent("Firewall");
  });

  it("network hosting lists a copyable address per adapter, the clone command and the firewall note", async () => {
    const h = await openProjects();
    act(() => h.emitServeState({ running: true, addr: "0.0.0.0:7878", dir: "C:\\srv\\p.rgit" }));
    const group = await screen.findByRole("group", { name: "Address for other devices" }, OPTS);
    await waitFor(() => expect(within(group).getAllByTestId("join-url")).toHaveLength(2), OPTS);
    expect(within(group).getAllByTestId("join-url").map((e) => e.textContent)).toEqual(["tcp://192.168.1.20:7878", "tcp://172.28.80.1:7878"]);
    expect(group).toHaveTextContent("rgit clone tcp://192.168.1.20:7878");
    expect(group).toHaveTextContent("Windows Defender Firewall");
    await h.user.click(within(group).getByRole("button", { name: "Copy tcp://192.168.1.20:7878" }));
    expect(await navigator.clipboard.readText()).toBe("tcp://192.168.1.20:7878"); // user-event's clipboard stand-in
    expect(await screen.findByText("Copied.")).toBeInTheDocument();
  });

  it("the network option is off by default, warns explicitly, and starts rgit with the wildcard address", async () => {
    let started: unknown = null;
    const h = await openProjects({
      serveStart: async (cfg: unknown) => {
        started = cfg;
        return { running: true, addr: "0.0.0.0:9000", dir: "C:\\srv\\p.rgit" };
      },
    });
    const box = screen.getByRole("checkbox", { name: "Allow other devices on this network" });
    expect(box).not.toBeChecked();
    expect(screen.queryByText(/Invitations and device authorization are NOT implemented/)).toBeNull();
    await h.user.type(screen.getByLabelText("Hosted repository folder"), "C:\\srv\\p.rgit");
    await h.user.click(box);
    expect(screen.getByRole("note", { name: "" })).toHaveTextContent("Connections stay encrypted");
    expect(screen.getByRole("note", { name: "" })).toHaveTextContent("enrolled device");
    const port = screen.getByLabelText("Port");
    await h.user.clear(port);
    await h.user.type(port, "9000");
    await h.user.click(screen.getByRole("button", { name: "Start hosting" }));
    await waitFor(() => expect(started).not.toBeNull(), OPTS);
    expect(started).toEqual({ dir: "C:\\srv\\p.rgit", init: true, listen: "0.0.0.0:9000", insecureListenAny: true, relay: undefined });
  });

  it("an invalid port keeps Start disabled", async () => {
    const h = await openProjects();
    await h.user.type(screen.getByLabelText("Hosted repository folder"), "C:\\srv\\p.rgit");
    const port = screen.getByLabelText("Port");
    await h.user.clear(port);
    await h.user.type(port, "99999x");
    expect(screen.getByRole("button", { name: "Start hosting" })).toBeDisabled();
  });
});

describe("History and merge", () => {
  it("lists commits with graph rows and shows commit details", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /History/ }));
    const list = await screen.findByRole("listbox", { name: "Commit history" }, OPTS);
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(8), OPTS);
    const merge = within(list).getByRole("option", { name: /Merge feature\/search into main/ });
    expect(within(merge).getByText("merge")).toBeInTheDocument();
    await h.user.click(within(list).getByRole("option", { name: /Add search index/ }));
    expect(await screen.findByText("Uses the new tokenizer.", {}, OPTS)).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Restore this version: docs/readme.md" }, OPTS)).toBeInTheDocument();
  });

  it("confirms before restoring a file and re-confirms when it has uncommitted edits", async () => {
    const h = renderApp({
      overrides: [
        { argv: ["restore", "--from", "*", "*"], file: "restore.dirty.json", exit: 1 },
        { argv: ["restore", "--from", "*", "--force", "*"], file: "restore.json" },
      ],
    });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /History/ }));
    const list = await screen.findByRole("listbox", { name: "Commit history" }, OPTS);
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(8), OPTS);
    await h.user.click(within(list).getByRole("option", { name: /Add search index/ }));
    await h.user.click(await screen.findByRole("button", { name: "Restore this version: docs/readme.md" }, OPTS));
    await h.user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Restore this version" }));
    const second = await screen.findByRole("dialog", {}, OPTS);
    expect(second).toHaveTextContent("uncommitted edits");
    await h.user.click(within(second).getByRole("button", { name: "Restore this version" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv.includes("--force"))).toBe(true), OPTS);
  });

  it("offers to set work aside when switching would overwrite it", async () => {
    const h = renderApp({ overrides: [{ argv: ["switch", "feature/search"], file: "switch.dirty.json", exit: 1 }] });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /History/ }));
    await h.user.click(await screen.findByRole("button", { name: "Switch: feature/search" }, OPTS));
    const dialog = await screen.findByRole("dialog", {}, OPTS);
    expect(dialog).toHaveTextContent("docs/readme.md");
    // the fake answers the second switch with the same error; only assert the shelve call happened first
    await h.user.click(within(dialog).getByRole("button", { name: "Set aside my changes and switch" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "shelve")).toBe(true), OPTS);
    expect(h.calls().find((c) => c.argv[0] === "shelve")!.argv[2]).toContain("feature/search");
  });

  it("resolves a merge conflict with 'keep theirs'", async () => {
    const h = renderApp({ overrides: [{ argv: ["status"], file: "status.merge.json" }] });
    await screen.findByText("Merge in progress: feature/search", {}, OPTS);
    const row = screen.getByText("docs/readme.md", { selector: ".conflict-row .path" }).closest("li")!;
    expect(within(row).getByText("Needs a decision")).toBeInTheDocument();
    await h.user.click(within(row).getByRole("button", { name: "Keep theirs: docs/readme.md" }));
    await waitFor(() => expect(h.calls().some((c) => c.argv[0] === "resolve")).toBe(true), OPTS);
    expect(h.calls().find((c) => c.argv[0] === "resolve")!.argv).toEqual(["resolve", "docs/readme.md", "--theirs"]);
    // binary conflicts cannot be hand-edited
    const bin = screen.getByText("logo.png", { selector: ".conflict-row .path" }).closest("li")!;
    expect(within(bin).queryByRole("button", { name: /I edited it/ })).toBeNull();
  });

  it("blocks committing a merge while conflicts are unresolved", async () => {
    const h = renderApp({ overrides: [{ argv: ["status"], file: "status.merge.json" }] });
    await screen.findByText("Merge in progress: feature/search", {}, OPTS);
    await h.user.type(screen.getByLabelText("Commit message"), "merge");
    expect(screen.getByRole("button", { name: "Commit and share" })).toBeDisabled();
  });
});

describe("Storage and recovery", () => {
  it("shows statistics, verifies with progress and reports problems", async () => {
    const h = renderApp({
      overrides: [
        { argv: ["verify"], file: "verify.problems.ndjson", exit: 2 },
        { argv: ["stats"], file: "stats.json" },
      ],
    });
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /Storage/ }));
    expect(await screen.findByText("5.6×", {}, OPTS)).toBeInTheDocument(); // 50 MiB logical / 9 MiB stored
    await h.user.click(screen.getByRole("button", { name: "Verify everything" }));
    const alert = await screen.findByText(/Verification found 1 problem/, {}, OPTS);
    expect(alert).toBeInTheDocument();
    expect(screen.getByText(/hash mismatch/)).toBeInTheDocument();
  });

  it("runs maintenance and reindex", async () => {
    const h = renderApp();
    await screen.findByText("docs/readme.md", {}, OPTS);
    await h.user.click(screen.getByRole("button", { name: /Storage/ }));
    await h.user.click(await screen.findByRole("button", { name: "Clean up and compress" }, OPTS));
    await h.user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Clean up and compress" }));
    expect(await screen.findByText(/Freed 1 MiB/, {}, OPTS)).toBeInTheDocument(); // gc.ndjson (hand-written) frees 1 MiB
    await h.user.click(screen.getByRole("button", { name: "Rebuild the index" }));
    expect(await screen.findByText(/Index rebuilt: 27 items in 1 pack file/, {}, OPTS)).toBeInTheDocument();
  });
});
