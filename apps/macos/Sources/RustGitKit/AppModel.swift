import AppKit
import Foundation
import Observation
import RgitClient

public enum EngineState: Equatable {
    case checking
    case ready(VersionInfo)
    case unavailable(UserFacingError)
}

public enum AppSection: String, CaseIterable, Identifiable {
    case projects, changes, history, sharing, storage
    public var id: String { rawValue }
}

@MainActor @Observable
public final class AppModel {
    public let settings: AppSettings
    public let loginItem = LoginItem()
    public private(set) var engine: EngineState = .checking
    public private(set) var client: RgitClient?
    public var project: ProjectModel?
    public var section: AppSection = .projects
    public var pendingInvitation: String?
    public private(set) var openingProjectPath: String?
    @ObservationIgnored private var openRequest = UUID()
    @ObservationIgnored private var projectCache: [String: ProjectModel] = [:]
    private let environment: [String: String]?
    public var banner: UserFacingError?
    public private(set) var hosts: HostManager!

    public init(settings: AppSettings? = nil, hostsDirectory: URL? = nil, environment: [String: String]? = nil) {
        self.environment = environment
        let settings = settings ?? AppSettings()
        self.settings = settings
        hosts = HostManager(settings: settings, directory: hostsDirectory, engine: { [weak self] in self?.client })
    }

    public func start() async {
        guard let exe = RgitLocator.find() else {
            engine = .unavailable(ErrorPresenter.present(RgitError.executableUnavailable(L("rgit was not found in the app bundle or PATH."))))
            return
        }
        let c = RgitClient(executable: exe, environment: environment)
        do {
            let v = try await c.checkCompatibility()
            client = c
            engine = .ready(v)
        } catch {
            engine = .unavailable(ErrorPresenter.present(error))
            return
        }
        await hosts.startAutoStarting()
        // Migrate only this app's old loopback connection. An existing pin is never replaced.
        for entry in hosts.entries {
            guard case .running = hosts.state(entry.id), let path = entry.projectPath,
                  FileManager.default.fileExists(atPath: path) else { continue }
            let projectClient = c.inDirectory(URL(fileURLWithPath: path))
            do {
                let remotes = try await projectClient.remotes()
                for remote in remotes where remote.url == "tcp://127.0.0.1:\(entry.port)" && remote.hostKey == nil {
                    guard let hosted = hosts.client(for: entry.id) else { continue }
                    let invitation = try await hosted.createInvitation(role: "admin", label: "Host owner upgrade", hints: [remote.url])
                    _ = try await projectClient.enroll(invitation: invitation.uri, remote: remote.name)
                }
            } catch { banner = ErrorPresenter.present(error) }
        }
        if let last = settings.lastProject, FileManager.default.fileExists(atPath: last) {
            await open(URL(fileURLWithPath: last), quiet: true)
        }
    }

    // MARK: Projects

    /// Opens an existing project folder. Returns false when it is not a RustGit project.
    @discardableResult
    public func open(_ url: URL, quiet: Bool = false) async -> Bool {
        guard let client else { return false }
        let url = url.standardizedFileURL
        let request = UUID()
        openRequest = request
        openingProjectPath = url.path
        defer { if openRequest == request { openingProjectPath = nil } }
        let p = projectCache[url.path] ?? ProjectModel(url: url, client: client)
        do {
            _ = try await p.client.status()
        } catch {
            if !quiet && openRequest == request { banner = ErrorPresenter.present(error) }
            return false
        }
        guard openRequest == request else { return false }
        banner = nil
        adopt(p)
        return true
    }

    public func create(at url: URL, branch: String, userName: String, userEmail: String) async {
        guard let client else { return }
        let c = client.inDirectory(url)
        do {
            _ = try await c.initProject(branch: branch)
            if !userName.isEmpty { try await c.setConfig("user.name", userName) }
            if !userEmail.isEmpty { try await c.setConfig("user.email", userEmail) }
        } catch { banner = ErrorPresenter.present(error); return }
        await open(url)
    }

    public func clone(address: String, into dest: URL, hostKey: String? = nil, progress: @escaping ProgressHandler) async -> Bool {
        guard let client else { return false }
        do {
            if address.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("projectapp://") {
                _ = try await client.join(invitation: address, into: dest, progress: progress)
            } else {
                _ = try await client.clone(url: address, into: dest, hostKey: hostKey, progress: progress)
            }
            pendingInvitation = nil
            await open(dest)
            return true
        } catch {
            banner = ErrorPresenter.present(error)
            return false
        }
    }

    private func adopt(_ p: ProjectModel) {
        project?.stopPolling()
        if let previous = project { projectCache[previous.id] = previous }
        projectCache[p.id] = p
        project = p
        settings.remember(p.url)
        section = .changes
        Task {
            await p.refresh()
            await p.refreshStorage()
            if self.project === p, p.hasRemote { p.startPolling(); await p.fetch() }
        }
    }

    public func close() {
        openRequest = UUID(); openingProjectPath = nil
        project?.stopPolling(); project = nil; section = .projects
        settings.lastProject = nil
    }

    // MARK: Hosting a project

    /// Creates a hosted copy, starts `rgit serve`, links it as a connection and uploads the current branch.
    public func hostProject(_ p: ProjectModel, startPort: Int = 7878, allowLAN: Bool = false) async {
        do {
            let entry = try await hosts.create(name: p.name, projectPath: p.url.path, startPort: startPort, allowLAN: allowLAN)
            let name = p.remotes.contains { $0.name == "origin" } ? "hosted" : "origin"
            if !p.remotes.contains(where: { $0.name == name }) {
                guard let hosted = hosts.client(for: entry.id) else { return }
                let invitation = try await hosted.createInvitation(role: "admin", label: "Host owner", hints: ["tcp://127.0.0.1:\(entry.port)"])
                _ = try await p.client.enroll(invitation: invitation.uri, remote: name)
                await p.refresh()
            }
            if p.status?.head != nil {
                if name == "origin" { await p.push() }
                else {
                    _ = await p.run(L("Sharing your commits…"), kind: .push, cancellable: true, refreshAfter: false) { c, prog in try await c.push(remote: name, progress: prog) }
                }
            }
            p.startPolling()
        } catch {
            p.fail(error)
        }
    }
}
