import Foundation
import Darwin

/// This Mac's usable IPv4 addresses, for showing "join" addresses.
public enum NetworkInfo {
    public struct Address: Equatable, Hashable, Sendable {
        public var interface: String
        public var ip: String
    }

    /// Up-and-running, non-loopback IPv4 addresses of physical-style interfaces (`getifaddrs`). Link-local 169.254.x.x addresses are skipped:
    /// they appear on interfaces without a real network and nobody can be told to join through them.
    /// Wi-Fi/Ethernet (`en*`) first.
    public static func lanIPv4Addresses() -> [Address] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return [] }
        defer { freeifaddrs(head) }
        var out: [Address] = []
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let ifa = cursor {
            defer { cursor = ifa.pointee.ifa_next }
            let flags = Int32(ifa.pointee.ifa_flags)
            // Only Wi-Fi/Ethernet/Thunderbolt-bridge style interfaces: VPN and proxy tunnels (utun*), AirDrop (awdl*),
            // low-latency Wi-Fi (llw*) and the like carry addresses other computers cannot be told to use.
            let name = String(cString: ifa.pointee.ifa_name)
            guard name.hasPrefix("en") || name.hasPrefix("bridge") else { continue }
            guard flags & IFF_UP != 0, flags & IFF_RUNNING != 0, flags & IFF_LOOPBACK == 0,
                  let sa = ifa.pointee.ifa_addr, sa.pointee.sa_family == sa_family_t(AF_INET) else { continue }
            var buf = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            let rc = getnameinfo(sa, socklen_t(sa.pointee.sa_len), &buf, socklen_t(buf.count), nil, 0, NI_NUMERICHOST)
            guard rc == 0 else { continue }
            let ip = String(cString: buf)
            guard !ip.hasPrefix("169.254."), ip != "0.0.0.0" else { continue }
            out.append(Address(interface: String(cString: ifa.pointee.ifa_name), ip: ip))
        }
        let unique = Array(Set(out))
        return unique.sorted { a, b in
            let ea = a.interface.hasPrefix("en"), eb = b.interface.hasPrefix("en")
            if ea != eb { return ea }
            return (a.interface, a.ip) < (b.interface, b.ip)
        }
    }
}

/// One address someone can join a hosted project through.
public struct JoinAddress: Identifiable, Equatable {
    public var id: String { url }
    public var label: String
    public var url: String
    /// Shell command that copies the project from this address into a new folder.
    public var cloneCommand: String { "rgit clone \(url) my-project" }
}

public enum JoinAddresses {
    /// Addresses for a hosted project listening on `listen` ("host:port"). The loopback address always works on
    /// this Mac; LAN addresses are included only when the host accepts other devices.
    public static func make(listen: String, allowLAN: Bool, lan: [NetworkInfo.Address] = NetworkInfo.lanIPv4Addresses()) -> [JoinAddress] {
        let port = listen.split(separator: ":").last.map(String.init) ?? "7878"
        var out = [JoinAddress(label: L("This Mac only"), url: "tcp://127.0.0.1:\(port)")]
        if allowLAN {
            for a in lan { out.append(JoinAddress(label: LF("Other devices on the network (%@)", a.interface), url: "tcp://\(a.ip):\(port)")) }
        }
        return out
    }
}
