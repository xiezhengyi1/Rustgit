import Foundation
import Observation
import RgitClient

/// Owns the `rgit serve` child processes. They run as long as the app process does (window-independent;
/// the menu-bar item keeps the app alive when the last window closes). Hosting is unavailable while the
/// Mac sleeps or the app is quit.
@MainActor @Observable
public final class HostManager {
    public enum State: Equatable {
        case stopped
        case starting
        case running(address: String)
        case failed(String)
    }

    public struct Activity: Identifiable, Equatable {
        public let id = UUID()
        public var date = Date()
        public var text: String
    }

    private let settings: AppSettings
    private let engine: () -> RgitClient?
    public private(set) var states: [UUID: State] = [:]
    public private(set) var activity: [UUID: [Activity]] = [:]
    public private(set) var fingerprints: [UUID: String] = [:]
    public private(set) var irohURLs: [UUID: String] = [:]
    private var sessions: [UUID: ServeSession] = [:]

    private let directory: URL

    public init(settings: AppSettings, directory: URL? = nil, engine: @escaping () -> RgitClient?) {
        self.settings = settings
        self.directory = directory ?? Self.hostsDirectory
        self.engine = engine
    }

    public var entries: [HostedProject] { settings.hosts }
    public var anyRunning: Bool { states.values.contains { if case .running = $0 { true } else { false } } }

    public func state(_ id: UUID) -> State { states[id] ?? .stopped }

    public func entry(forProject path: String) -> HostedProject? { settings.hosts.first { $0.projectPath == path } }

    public static var hostsDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("RustGit/Hosts", isDirectory: true)
    }

    /// Creates a hosted repository, starts serving it on the first free port from `startPort`.
    @discardableResult
    public func create(name: String, projectPath: String?, startPort: Int = 7878, allowLAN: Bool = false) async throws -> HostedProject {
        guard let client = engine() else { throw RgitError.executableUnavailable("rgit not found") }
        let id = UUID()
        let safe = name.replacingOccurrences(of: "/", with: "-")
        let dir = directory.appendingPathComponent("\(safe)-\(id.uuidString.prefix(8)).rgit", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var lastError: Error?
        for port in startPort..<(startPort + 10) {
            let listen = "\(allowLAN ? "0.0.0.0" : "127.0.0.1"):\(port)"
            do {
                states[id] = .starting
                let s = try await ServeSession.start(executable: client.executable, directory: dir,
                                                     options: .init(listen: listen, initialize: true, advertise: allowLAN), environment: client.environment)
                let entry = HostedProject(id: id, name: name, path: dir.path, listen: listen, projectPath: projectPath, autoStart: true, allowLAN: allowLAN)
                settings.hosts.append(entry)
                attach(s, to: id)
                return entry
            } catch let e as RgitError {
                lastError = e
                // Port taken: try the next one. Anything else is a real error.
                let msg = e.errorDescription?.lowercased() ?? ""
                if !(msg.contains("in use") || msg.contains("address")) { break }
            }
        }
        states[id] = nil
        throw lastError ?? RgitError.malformedOutput("could not start host")
    }

    public func start(_ id: UUID) async {
        guard let entry = settings.hosts.first(where: { $0.id == id }), sessions[id] == nil, let client = engine() else { return }
        states[id] = .starting
        do {
            let s = try await ServeSession.start(executable: client.executable, directory: URL(fileURLWithPath: entry.path),
                                                 options: .init(listen: entry.listen, advertise: entry.allowLAN, relay: entry.relay), environment: client.environment)
            attach(s, to: id)
        } catch {
            states[id] = .failed(ErrorPresenter.present(error).detail)
        }
    }

    public func stop(_ id: UUID) async {
        guard let s = sessions[id] else { states[id] = .stopped; return }
        sessions[id] = nil
        irohURLs[id] = nil
        await s.stop()
        states[id] = .stopped
    }

    public func stopAll() async {
        for id in Array(sessions.keys) { await stop(id) }
    }

    /// Synchronous kill for `applicationWillTerminate`.
    public func terminateAllNow() {
        for s in sessions.values where s.isRunning { s.process.terminate() }
    }

    public func remove(_ id: UUID, deleteFiles: Bool) async {
        await stop(id)
        if deleteFiles, let e = settings.hosts.first(where: { $0.id == id }) {
            try? FileManager.default.removeItem(atPath: e.path)
        }
        settings.hosts.removeAll { $0.id == id }
        states[id] = nil
        activity[id] = nil
    }

    public func startAutoStarting() async {
        for e in settings.hosts where e.autoStart { await start(e.id) }
    }

    /// Switches between "this Mac only" and "other devices on the network". The host restarts if it was running.
    /// Only ever turned on by an explicit user action; the transport remains encrypted and requires device enrollment.
    public func setAllowLAN(_ id: UUID, _ on: Bool) async {
        guard let i = settings.hosts.firstIndex(where: { $0.id == id }), settings.hosts[i].allowLAN != on else { return }
        let wasRunning: Bool = { if case .running = state(id) { true } else { false } }()
        await stop(id)
        settings.hosts[i].allowLAN = on
        settings.hosts[i].listen = "\(on ? "0.0.0.0" : "127.0.0.1"):\(settings.hosts[i].port)"
        if wasRunning { await start(id) }
    }

    /// Addresses for joining this hosted project.
    public func joinAddresses(_ id: UUID) -> [JoinAddress] {
        guard let e = settings.hosts.first(where: { $0.id == id }) else { return [] }
        return JoinAddresses.make(listen: e.listen, allowLAN: e.allowLAN)
    }

    public func client(for id: UUID) -> RgitClient? {
        guard let entry = entries.first(where: { $0.id == id }) else { return nil }
        return engine()?.inDirectory(URL(fileURLWithPath: entry.path))
    }

    public func setRelay(_ id: UUID, _ relay: String) async {
        guard let i = settings.hosts.firstIndex(where: { $0.id == id }) else { return }
        let value = relay.trimmingCharacters(in: .whitespacesAndNewlines)
        if !value.isEmpty {
            guard let u = URL(string: value), ["https", "http"].contains(u.scheme ?? ""), u.host != nil,
                  u.user == nil, u.password == nil else {
                log(id, L("Enter an HTTP or HTTPS relay URL without credentials.")); return
            }
        }
        await stop(id)
        settings.hosts[i].relay = value.isEmpty ? nil : value
        await start(id)
    }

    public func setAutoStart(_ id: UUID, _ on: Bool) {
        if let i = settings.hosts.firstIndex(where: { $0.id == id }) { settings.hosts[i].autoStart = on }
    }

    private func attach(_ s: ServeSession, to id: UUID) {
        sessions[id] = s
        fingerprints[id] = s.fingerprint
        states[id] = .running(address: s.address)
        log(id, LF("Listening on %@", s.address))
        Task { [weak self] in
            for await ev in s.events {
                guard let self else { return }
                switch ev {
                case .connection(let t): self.log(id, t)
                case .push(let t): self.log(id, t)
                case .error(let t): self.log(id, t)
                case .other(let event, let line):
                    if event == "listening_iroh", let data = line.data(using: .utf8),
                       let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                       let url = obj["url"] as? String {
                        self.irohURLs[id] = url
                        if obj["relay_online"] as? Bool == false { self.log(id, L("The relay is not reachable yet. The host will keep trying.")) }
                    }
                case .listening: break
                case .exited(let status):
                    // Ignore the exit we asked for.
                    if self.sessions[id] === s {
                        self.sessions[id] = nil
                        self.states[id] = .failed(LF("The host stopped unexpectedly (status %d).", Int(status)))
                    }
                }
            }
        }
    }

    private func log(_ id: UUID, _ text: String) {
        var list = activity[id] ?? []
        list.append(Activity(text: text))
        if list.count > 50 { list.removeFirst(list.count - 50) }
        activity[id] = list
    }
}
