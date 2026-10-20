import Foundation
import Observation
import ServiceManagement

/// A project the app has opened before (folder path only; the repository itself lives in `.rgit`).
public struct KnownProject: Codable, Identifiable, Hashable {
    public var path: String
    public var id: String { path }
    public var name: String { URL(fileURLWithPath: path).lastPathComponent }
}

/// A hosted (bare) repository this app serves with `rgit serve`.
public struct HostedProject: Codable, Identifiable, Hashable {
    public var id: UUID
    public var name: String
    public var path: String
    public var listen: String
    /// Working project whose branch was published here, if any.
    public var projectPath: String?
    public var autoStart: Bool
    /// Accept connections from other computers (`--listen 0.0.0.0`, authenticated). Off by default.
    public var allowLAN: Bool
    public var relay: String?

    public init(id: UUID, name: String, path: String, listen: String, projectPath: String?, autoStart: Bool, allowLAN: Bool = false, relay: String? = nil) {
        self.id = id; self.name = name; self.path = path; self.listen = listen
        self.projectPath = projectPath; self.autoStart = autoStart; self.allowLAN = allowLAN; self.relay = relay
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(UUID.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        path = try c.decode(String.self, forKey: .path)
        listen = try c.decode(String.self, forKey: .listen)
        projectPath = try c.decodeIfPresent(String.self, forKey: .projectPath)
        autoStart = try c.decodeIfPresent(Bool.self, forKey: .autoStart) ?? true
        relay = try c.decodeIfPresent(String.self, forKey: .relay)
        allowLAN = try c.decodeIfPresent(Bool.self, forKey: .allowLAN) ?? false     // hosts saved before this existed
    }

    public var port: String { listen.split(separator: ":").last.map(String.init) ?? "7878" }
}

@MainActor @Observable
public final class AppSettings {
    private let defaults: UserDefaults

    public var keepHostingInMenuBar: Bool { didSet { defaults.set(keepHostingInMenuBar, forKey: "keepHostingInMenuBar") } }
    public var projects: [KnownProject] { didSet { save(projects, "projects") } }
    public var hosts: [HostedProject] { didSet { save(hosts, "hosts") } }
    public var lastProject: String? { didSet { defaults.set(lastProject, forKey: "lastProject") } }

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        keepHostingInMenuBar = defaults.object(forKey: "keepHostingInMenuBar") as? Bool ?? true
        projects = Self.load(defaults, "projects") ?? []
        hosts = Self.load(defaults, "hosts") ?? []
        lastProject = defaults.string(forKey: "lastProject")
    }

    private func save<T: Encodable>(_ v: T, _ key: String) {
        if let d = try? JSONEncoder().encode(v) { defaults.set(d, forKey: key) }
    }
    private static func load<T: Decodable>(_ defaults: UserDefaults, _ key: String) -> T? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
    }

    public func remember(_ url: URL) {
        let p = url.standardizedFileURL.path
        projects.removeAll { $0.path == p }
        projects.insert(KnownProject(path: p), at: 0)
        if projects.count > 30 { projects.removeLast(projects.count - 30) }
        lastProject = p
    }

    public func forget(_ path: String) { projects.removeAll { $0.path == path } }
}

/// Start-at-login through `SMAppService`. Only meaningful for a signed `.app` bundle in a
/// stable location (normally /Applications); from `swift run` there is no bundle and it reports
/// `unavailable`. Registration is always an explicit user action and shows up in System Settings >
/// General > Login Items; nothing privileged is installed.
@MainActor @Observable
public final class LoginItem {
    public enum State: Equatable { case unavailable(String), off, on, needsApproval }
    public private(set) var state: State = .off
    public var lastError: String?

    public init() { refresh() }

    public func refresh() {
        guard Bundle.main.bundleIdentifier != nil, Bundle.main.bundleURL.pathExtension == "app" else {
            state = .unavailable(L("Only available when running the packaged app."))
            return
        }
        switch SMAppService.mainApp.status {
        case .enabled: state = .on
        case .requiresApproval: state = .needsApproval
        case .notRegistered: state = .off
        case .notFound: state = .unavailable(L("macOS cannot find this app. Move it to the Applications folder."))
        @unknown default: state = .off
        }
    }

    public func set(_ on: Bool) {
        lastError = nil
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            lastError = error.localizedDescription
        }
        refresh()
    }
}
