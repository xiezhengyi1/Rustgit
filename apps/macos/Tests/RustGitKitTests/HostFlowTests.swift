import Foundation
import Testing
import RgitClient
@testable import RustGitKit

/// End to end: the app hosts a project (separate bare repository, `serve`), other folders clone from the
/// displayed addresses, push, and the original fetches. Needs RGIT_BINARY.
@MainActor
@Suite("Hosting flow", .serialized)
struct HostFlowTests {
    func sandbox() throws -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("hostflow-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    @Test func hostCloneFromDisplayedAddressesPushFetch() async throws {
        guard ProcessInfo.processInfo.environment["RGIT_BINARY"] != nil else { return }
        let defaults = UserDefaults(suiteName: "hostflow-\(UUID().uuidString)")!
        let box = try sandbox(); defer { try? FileManager.default.removeItem(at: box) }
        let model = AppModel(settings: AppSettings(defaults: defaults), hostsDirectory: box.appendingPathComponent("hosts"), environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("owner-keys").path])
        do { try await flow(model, box) } catch { await model.hosts.stopAll(); throw error }
        await model.hosts.stopAll()
    }

    func flow(_ model: AppModel, _ box: URL) async throws {
        await model.start()
        let exe = try #require(model.client).executable

        // A working project with history.
        let proj = box.appendingPathComponent("My Project"); try FileManager.default.createDirectory(at: proj, withIntermediateDirectories: true)
        await model.create(at: proj, branch: "main", userName: "Host Owner", userEmail: "o@example.com")
        let p = try #require(model.project)
        try "first\n".write(to: proj.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        await p.refresh()
        #expect(await p.commit(message: "first"))

        // Host it: default is this Mac only.
        await model.hostProject(p, startPort: 18_700 + Int.random(in: 0..<800))
        #expect(p.error == nil, "\(String(describing: p.error))")
        let entry = try #require(model.hosts.entry(forProject: proj.path))
        #expect(!entry.allowLAN && entry.listen.hasPrefix("127.0.0.1:"))
        #expect(entry.path != proj.path)                       // a separate bare repository, never the working folder
        let port = entry.port
        var addrs = model.hosts.joinAddresses(entry.id)
        #expect(addrs.map(\.url) == ["tcp://127.0.0.1:\(port)"])
        #expect(p.remotes.map(\.url) == ["tcp://127.0.0.1:\(port)"])
        #expect(p.stage(.receivedByServer) || p.lastPushedHead != nil)

        let lan = NetworkInfo.lanIPv4Addresses()
        print("HOSTFLOW LAN addresses on this Mac: \(lan.map { "\($0.interface) \($0.ip)" })")

        // With LAN off, the LAN address must refuse connections.
        if let ip = lan.first?.ip {
            do {
                _ = try await RgitClient(executable: exe).clone(url: "tcp://\(ip):\(port)", into: box.appendingPathComponent("denied"))
                Issue.record("LAN address must be unreachable while 'allow other devices' is off")
            } catch let e as RgitError { print("HOSTFLOW LAN off -> \(e.code?.wire ?? "\(e)")") }
        }

        let hosted = try #require(model.hosts.client(for: entry.id))
        let identity = try await hosted.hostIdentity()
        #expect(model.hosts.fingerprints[entry.id] == identity.fingerprint)
        let guest = RgitClient(executable: exe, environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("guest-keys").path])
        let firstInvite = try await hosted.createInvitation(role: "write", hints: [addrs[0].url])
        // Join by invitation from the displayed loopback address.
        let c1 = box.appendingPathComponent("clone-loopback")
        _ = try await guest.join(invitation: firstInvite.uri, into: c1)
        #expect(try String(contentsOf: c1.appendingPathComponent("a.txt"), encoding: .utf8) == "first\n")

        // Turn on LAN; the host restarts on the same port and shows LAN join addresses.
        await model.hosts.setAllowLAN(entry.id, true)
        #expect(model.hosts.state(entry.id) == .running(address: "0.0.0.0:\(port)"))
        addrs = model.hosts.joinAddresses(entry.id)
        print("HOSTFLOW join addresses: \(addrs.map(\.url))")
        #expect(addrs.count == 1 + lan.count)
        guard let joinURL = addrs.last?.url, !lan.isEmpty else { print("HOSTFLOW no LAN address on this machine: LAN clone not exercised"); await model.hosts.stopAll(); return }

        let c2 = box.appendingPathComponent("clone-lan")
        let cl = RgitClient(executable: exe, environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("guest-lan-keys").path])
        let invite = try await hosted.createInvitation(role: "write", label: "LAN guest", hints: [joinURL])
        let joined = try await cl.join(invitation: invite.uri, into: c2)
        #expect(joined.hostFingerprint == identity.fingerprint)
        let c2c = cl.inDirectory(c2)
        try await c2c.setConfig("user.name", "Guest"); try await c2c.setConfig("user.email", "g@example.com")
        try "from guest\n".write(to: c2.appendingPathComponent("g.txt"), atomically: true, encoding: .utf8)
        _ = try await c2c.commit(message: "guest work")
        let pushed = try await c2c.push()
        #expect(!pushed.upToDate)

        // The host owner's project fetches it through the same connection.
        await p.fetch()
        #expect(p.syncState == .behind)
        await p.pull()
        #expect(try String(contentsOf: proj.appendingPathComponent("g.txt"), encoding: .utf8) == "from guest\n")

        // Revoke the guest: its pinned encrypted remote must stop working.
        try await hosted.revokeDevice(joined.deviceFingerprint)
        do { _ = try await c2c.fetch(); Issue.record("a revoked guest must lose access") }
        catch let error as RgitError { #expect(error.code?.wire == "device_revoked") }
        #expect(try await hosted.invitations().invitations.contains { $0.id == invite.id && $0.status == "used" })

        // Turning LAN off again closes the address.
        await model.hosts.setAllowLAN(entry.id, false)
        do { _ = try await cl.clone(url: joinURL, into: box.appendingPathComponent("denied2")); Issue.record("must be unreachable again") }
        catch let e as RgitError { print("HOSTFLOW LAN off again -> \(e.code?.wire ?? "\(e)")") }
        await model.hosts.stopAll()
    }

    @Test func upgradesTheOwnersUnpinnedLoopbackConnectionWithoutReplacingAPin() async throws {
        guard ProcessInfo.processInfo.environment["RGIT_BINARY"] != nil else { return }
        let box = try sandbox(); defer { try? FileManager.default.removeItem(at: box) }
        let settings = AppSettings(defaults: UserDefaults(suiteName: "upgrade-\(UUID().uuidString)")!)
        let env = ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("keys").path]
        let model = AppModel(settings: settings, hostsDirectory: box.appendingPathComponent("hosts"), environment: env)
        await model.start()
        let path = box.appendingPathComponent("work")
        try FileManager.default.createDirectory(at: path, withIntermediateDirectories: true)
        await model.create(at: path, branch: "main", userName: "Owner", userEmail: "o@example.com")
        let project = try #require(model.project)
        let entry = try await model.hosts.create(name: "old host", projectPath: path.path, startPort: 22_000 + Int.random(in: 0..<1000))
        try await project.client.addRemote(name: "origin", url: "tcp://127.0.0.1:\(entry.port)")
        await model.hosts.stopAll(); model.close()
        await model.start()
        let upgraded = try await project.client.remotes().first
        #expect(upgraded?.secure == true)
        #expect(upgraded?.hostKey == model.hosts.fingerprints[entry.id])
        await model.hosts.stopAll(); model.close()
        let wrongPin = String(repeating: "f", count: 64)
        try await project.client.addRemote(name: "origin", url: "tcp://127.0.0.1:\(entry.port)", hostKey: wrongPin)
        await model.start()
        #expect(try await project.client.remotes().first?.hostKey == wrongPin)
        await model.hosts.stopAll(); model.close()
    }

    @Test func joinAddressListsAreExactlyWhatTheHostAccepts() {
        let lan = [NetworkInfo.Address(interface: "en0", ip: "192.168.1.20"), NetworkInfo.Address(interface: "en5", ip: "10.0.0.7")]
        #expect(JoinAddresses.make(listen: "127.0.0.1:7878", allowLAN: false, lan: lan).map(\.url) == ["tcp://127.0.0.1:7878"])
        #expect(JoinAddresses.make(listen: "0.0.0.0:7880", allowLAN: true, lan: lan).map(\.url)
                == ["tcp://127.0.0.1:7880", "tcp://192.168.1.20:7880", "tcp://10.0.0.7:7880"])
        #expect(JoinAddresses.make(listen: "0.0.0.0:7880", allowLAN: true, lan: lan).last?.cloneCommand == "rgit clone tcp://10.0.0.7:7880 my-project")
    }

    @Test func hostsSavedBeforeLANExistedStillDecodeAsLoopbackOnly() throws {
        let old = #"{"id":"6F9619FF-8B86-D011-B42D-00C04FC964FF","name":"x","path":"/p","listen":"127.0.0.1:7878","autoStart":true}"#
        let h = try JSONDecoder().decode(HostedProject.self, from: Data(old.utf8))
        #expect(!h.allowLAN && h.port == "7878")
    }

    @Test func networkAddressesAreRealAndNotLoopback() {
        for a in NetworkInfo.lanIPv4Addresses() { #expect(!a.ip.hasPrefix("127.") && !a.ip.hasPrefix("169.254.")) }
    }
}

@Suite("Invitations for another computer")
struct SharingInvitationHintTests {
    @Test func localInvitationsExcludeLoopbackAndWildcardAddresses() {
        let result = SharingInvitationHints.make(addresses: ["tcp://127.0.0.1:7878", "tcp://0.0.0.0:7878", "tcp://192.168.1.20:7878", "tcp://10.0.0.5:7878"], internetURL: nil, viaInternet: false)
        #expect(result == ["tcp://192.168.1.20:7878", "tcp://10.0.0.5:7878"])
        #expect(SharingInvitationHints.make(addresses: ["tcp://127.0.0.1:7878"], internetURL: nil, viaInternet: false).isEmpty)
    }
    @Test func internetInvitationsUseTheInternetEndpointOnly() {
        let url = "iroh://endpoint?relay=https://relay.example"
        #expect(SharingInvitationHints.make(addresses: ["tcp://192.168.1.20:7878"], internetURL: url, viaInternet: true) == [url])
        #expect(SharingInvitationHints.make(addresses: ["tcp://192.168.1.20:7878"], internetURL: nil, viaInternet: true).isEmpty)
    }
}
