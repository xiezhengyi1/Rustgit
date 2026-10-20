import { describe, expect, it } from "vitest";
import { cloneCommand, isValidPort, joinAddresses, listenFor, portOf } from "../src/rgit/hosting";

describe("hosting helpers", () => {
  it("builds the listen address from the network option", () => {
    expect(listenFor(false, 7878)).toBe("127.0.0.1:7878");
    expect(listenFor(true, "9000")).toBe("0.0.0.0:9000");
    expect(portOf("0.0.0.0:9000")).toBe("9000");
    expect(portOf("nonsense")).toBeNull();
    expect(isValidPort("0")).toBe(true);
    expect(isValidPort("65536")).toBe(false);
    expect(isValidPort("80a")).toBe(false);
  });
  it("loopback is local only; wildcard expands to LAN addresses; a specific bind is used as is", () => {
    const lan = [{ ip: "192.168.0.5", name: "Ethernet", private: true }];
    expect(joinAddresses("127.0.0.1:7878", lan)).toMatchObject({ local: true, list: [{ url: "tcp://127.0.0.1:7878" }] });
    const wild = joinAddresses("0.0.0.0:7878", lan);
    expect(wild.local).toBe(false);
    expect(wild.list).toEqual([{ url: "tcp://192.168.0.5:7878", label: "Ethernet", clone: cloneCommand("tcp://192.168.0.5:7878") }]);
    expect(joinAddresses("10.0.0.9:1", lan).list[0]!.url).toBe("tcp://10.0.0.9:1");
    expect(joinAddresses(null, lan).list).toEqual([]);
    expect(joinAddresses("0.0.0.0:7878", []).list).toEqual([]);
  });
});
