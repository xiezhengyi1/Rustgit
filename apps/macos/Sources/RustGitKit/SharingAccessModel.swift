import Foundation
import Observation
import RgitClient

@MainActor @Observable
final class SharingAccessModel {
    var identity: DeviceIdentity?
    var invitations: [Invitation] = []
    var devices: [EnrolledDevice] = []
    var nearby: [DiscoveredHost] = []
    var error: UserFacingError?
    var busy = false
    // Kept only in the current view, never preferences or the host log.
    var invitation: String?

    func perform(_ action: () async throws -> Void) async {
        guard !busy else { return }
        busy = true; error = nil
        defer { busy = false }
        do { try await action() } catch { self.error = ErrorPresenter.present(error) }
    }
    func refresh(device: RgitClient?, host: RgitClient?) async {
        await perform {
            if let device { identity = try await device.deviceIdentity() }
            if let host {
                invitations = try await host.invitations().invitations
                devices = try await host.devices().devices
            }
        }
    }
    func watch(_ client: RgitClient) async {
        do {
            try await client.watchDiscovery { [weak self] event in
                Task { @MainActor in
                    guard let self else { return }
                    if let host = event.host {
                        self.nearby.removeAll { $0.id == host.id }; self.nearby.append(host)
                    } else if event.event == "lost", let id = event.raw["instance"] {
                        self.nearby.removeAll { $0.id == id }
                    }
                }
            }
        } catch {
            if !Task.isCancelled { self.error = ErrorPresenter.present(error) }
        }
    }
}
