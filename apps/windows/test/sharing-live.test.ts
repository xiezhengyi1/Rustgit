import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { expect, it } from "vitest";
import { RgitClient } from "../src/rgit/client";
import { nodeRunner } from "./node-runner";

it("joins securely through stdin, enforces roles and revokes a real device", async () => {
  const root = mkdtempSync(join(tmpdir(), "rgit-sharing-"));
  const bin = resolve(process.env.RGIT_BIN ?? "../../target/release/rgit");
  const env = (key: string) => ({ RGIT_INSECURE_FILE_KEYS: "1", RGIT_KEY_DIR: join(root, key) });
  const owner = new RgitClient(nodeRunner({ realBinary: bin, environment: env("owner") }));
  const guestRunner = nodeRunner({ realBinary: bin, environment: env("guest") });
  let joinArgs: string[] = [];
  const guest = new RgitClient({ spawn: req => { if (req.args[0] === "join") joinArgs = req.args; return guestRunner.spawn(req); } });
  const hostDir = join(root, "host");
  const host = spawn(bin, ["--json", "serve", hostDir, "--init", "--listen", "127.0.0.1:0"], { env: { ...process.env, ...env("owner") } });
  try {
    const listening = await new Promise<{ addr: string; fingerprint: string; secure: boolean }>((res, rej) => {
      let buf = "";
      const timer = setTimeout(() => rej(new Error("host startup timeout")), 10000);
      host.once("exit", code => { clearTimeout(timer); rej(new Error(`host exited ${code}`)); });
      host.stdout.on("data", chunk => {
        buf += String(chunk);
        let nl;
        while ((nl = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
          const event = JSON.parse(line);
          if (event.event === "listening") { clearTimeout(timer); res(event); }
        }
      });
    });
    expect(listening.secure).toBe(true);
    const invitation = await owner.createInvitation(hostDir, "write", "24h", true, "Guest", [`tcp://${listening.addr}`]);
    const dest = join(root, "copy");
    const joined = await guest.join(invitation.uri, dest);
    expect(joinArgs).toEqual(["join", "-", dest]);
    expect(JSON.stringify(joinArgs)).not.toContain(invitation.uri);
    expect(joined.host_fingerprint).toBe(listening.fingerprint);
    expect((await guest.remotes(dest)).remotes[0]?.secure).toBe(true);
    await guest.call(["config", "user.name", "Guest"], { project: dest });
    writeFileSync(join(dest, "work.txt"), "work\n");
    await guest.commit(dest, "first"); await guest.push(dest);
    await owner.setDeviceRole(hostDir, joined.device_fingerprint, "read");
    writeFileSync(join(dest, "work.txt"), "more\n");
    await guest.commit(dest, "second");
    await expect(guest.push(dest)).rejects.toMatchObject({ code: "permission_denied" });
    await owner.revokeDevice(hostDir, joined.device_fingerprint);
    await expect(guest.fetch(dest)).rejects.toMatchObject({ code: "device_revoked" });
    expect((await owner.devices(hostDir)).devices[0]?.status).toBe("revoked");
    const ownerProject = join(root, "owner-project");
    mkdirSync(ownerProject);
    await owner.init(ownerProject);
    const ownerInvite = await owner.createInvitation(hostDir, "admin", "24h", true, "Host owner", [`tcp://${listening.addr}`]);
    await owner.enroll(ownerProject, ownerInvite.uri);
    expect((await owner.remotes(ownerProject)).remotes[0]?.host_key).toBe(listening.fingerprint);
    const unused = await owner.createInvitation(hostDir, "read", "24h", true, "Unused", [`tcp://${listening.addr}`]);
    await owner.revokeInvitation(hostDir, unused.id);
    expect((await owner.invitations(hostDir)).invitations.find(i => i.id === unused.id)?.status).toBe("revoked");
  } finally {
    const exited = new Promise<void>(res => host.once("exit", () => res()));
    host.kill(); await exited;
    rmSync(root, { recursive: true, force: true });
  }
}, 30000);
