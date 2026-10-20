import Foundation
import Testing
@testable import RgitClient

@Suite("Authenticated sharing")
struct SharingTests {
    @Test func invitationJoinRoleAndRevocation() async throws {
        let box = try Sandbox()
        func client(_ key: String, _ directory: URL? = nil) -> RgitClient {
            RgitClient(executable: TestEnv.rgit, directory: directory, environment: [
                "RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.url.appendingPathComponent(key).path
            ])
        }
        let hostDir = try box.dir("host")
        let owner = client("owner", hostDir)
        let session = try await ServeSession.start(executable: TestEnv.rgit, directory: hostDir,
            options: .init(listen: "127.0.0.1:0", initialize: true), environment: owner.environment)
        do {
            let url = "tcp://\(session.address)"
            let identity = try await owner.hostIdentity()
            #expect(session.fingerprint == identity.fingerprint)
            let invite = try await owner.createInvitation(role: "write", label: "Guest", hints: [url])
            let dest = box.url.appendingPathComponent("copy")
            let guest = client("guest")
            let joined = try await guest.join(invitation: invite.uri, into: dest)
            let work = guest.inDirectory(dest)
            #expect(joined.hostFingerprint == identity.fingerprint)
            #expect(try await work.remotes().first?.secure == true)
            #expect(try await guest.deviceIdentity().fingerprint == joined.deviceFingerprint)
            try await work.setConfig("user.name", "Guest")
            try Sandbox.write("guest work\n", to: "work.txt", in: dest)
            _ = try await work.commit(message: "guest")
            _ = try await work.push()
            try await owner.setDeviceRole(joined.deviceFingerprint, role: "read")
            try Sandbox.write("more\n", to: "work.txt", in: dest)
            _ = try await work.commit(message: "cannot push")
            do { _ = try await work.push(); Issue.record("read-only push must fail") }
            catch let error as RgitError { #expect(error.code == .permissionDenied) }
            try await owner.revokeDevice(joined.deviceFingerprint)
            do { _ = try await work.fetch(); Issue.record("revoked device must fail") }
            catch let error as RgitError { #expect(error.code?.wire == "device_revoked") }
            let unused = try await owner.createInvitation(hints: [url])
            try await owner.revokeInvitation(unused.id)
            #expect(try await owner.invitations().invitations.contains { $0.id == unused.id && $0.status == "revoked" })
            #expect(try await owner.devices().devices.contains { $0.fingerprint == joined.deviceFingerprint && $0.status == "revoked" })
        } catch { await session.stop(); throw error }
        await session.stop()
    }

    @Test func connectionAndDiscoveryEventsDecode() throws {
        let line = Data(#"{"event":"connection","state":"relayed","path":"relay https://relay.example/","used_relay":true,"path_switches":2}"#.utf8)
        guard case .event(let event) = WireLine.parse(line) else { Issue.record("missing event"); return }
        #expect(event.connection?.state == "relayed")
        #expect(event.connection?.usedRelay == true)
        #expect(event.connection?.pathSwitches == 2)
        guard case .event(let found) = WireLine.parse(Data(#"{"event":"host","host":{"instance":"test","urls":["tcp://192.0.2.1:7878"],"verified":false}}"#.utf8)) else { Issue.record("missing host"); return }
        #expect(found.host?.verified == false)
        #expect(found.host?.urls.count == 1)
    }
}
