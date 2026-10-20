import AppKit
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var signalSources: [DispatchSourceSignal] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        // A bare executable (swift run) has no Info.plist: make it a regular GUI app anyway.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        // `kill`/logout send SIGTERM, which skips applicationWillTerminate: route it through a normal quit so the
        // hosted `rgit serve` children are stopped instead of being left running without the app.
        for sig in [SIGTERM, SIGINT, SIGHUP] {
            signal(sig, SIG_IGN)
            let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            src.setEventHandler { NSApp.terminate(nil) }
            src.resume()
            signalSources.append(src)
        }
    }

    /// Closing the window keeps the app (and hosting) alive when the menu-bar option is on and a host is running.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        let m = AppModel.shared
        return !(m.settings.keepHostingInMenuBar && m.hosts.anyRunning)
    }

    func applicationWillTerminate(_ notification: Notification) {
        AppModel.shared.hosts.terminateAllNow()
    }
}

public struct RustGitApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let model = AppModel.shared

    public init() {}

    public var body: some Scene {
        Window("RustGit", id: "main") {
            RootView()
                .environment(model)
                .task { await model.start() }
                .onOpenURL { url in
                    if url.scheme == "projectapp", url.host == "join" { model.pendingInvitation = url.absoluteString; model.section = .projects }
                }
        }
        .defaultSize(width: 1100, height: 720)
        .commands {
            CommandGroup(after: .newItem) {
                Button(L("Open Project Folder…")) {
                    if let u = chooseFolder(prompt: L("Open"), canCreate: false) { Task { await model.open(u) } }
                }.keyboardShortcut("o")
                Button(L("Check for Updates from Host")) { Task { await model.project?.fetch() } }
                    .keyboardShortcut("r", modifiers: [.command, .shift])
                    .disabled(model.project?.hasRemote != true)
            }
        }

        MenuBarExtra {
            MenuBarContent().environment(model)
        } label: {
            Image(systemName: model.hosts.anyRunning ? "antenna.radiowaves.left.and.right" : "arrow.triangle.branch")
                .accessibilityLabel(model.hosts.anyRunning ? L("RustGit, hosting") : L("RustGit"))
        }

        Settings {
            SettingsView().environment(model)
        }
    }
}

extension AppModel {
    @MainActor static let shared = AppModel()
}

struct MenuBarContent: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        @Bindable var settings = app.settings
        let hosts = app.hosts!
        Button(L("Open RustGit")) {
            openWindow(id: "main")
            NSApp.activate(ignoringOtherApps: true)
        }
        Divider()
        if hosts.entries.isEmpty {
            Text(L("No hosted projects"))
        }
        ForEach(hosts.entries) { e in
            let running: Bool = { if case .running = hosts.state(e.id) { true } else { false } }()
            Toggle(isOn: Binding(get: { running }, set: { on in Task { if on { await hosts.start(e.id) } else { await hosts.stop(e.id) } } })) {
                Text(running ? LF("%@ — hosting on %@", e.name, hosts.joinAddresses(e.id).last?.url.replacingOccurrences(of: "tcp://", with: "") ?? e.listen) : LF("%@ — stopped", e.name))
            }
        }
        Divider()
        Toggle(L("Keep hosting when the window is closed"), isOn: $settings.keepHostingInMenuBar)
        Text(L("Hosting stops when the Mac sleeps or shuts down.")).font(.caption)
        Divider()
        Button(L("Quit RustGit")) { NSApp.terminate(nil) }.keyboardShortcut("q")
    }
}

struct SettingsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var settings = app.settings
        Form {
            Section(L("Hosting")) {
                Toggle(L("Keep hosting in the menu bar when the window is closed"), isOn: $settings.keepHostingInMenuBar)
                loginToggle
                Text(L("Hosting is only available while this Mac is awake and RustGit is running. Nothing is installed with administrator rights.")).font(.caption).foregroundStyle(.secondary)
            }
            Section(L("Engine")) {
                switch app.engine {
                case .ready(let v): Text(LF("rgit %@ · interface %d · format %d", v.cli, v.jsonInterface, v.format)).font(.callout.monospaced())
                case .checking: Text(L("Starting the engine…"))
                case .unavailable(let e): Text(e.title).foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped).frame(width: 480).padding()
    }

    @ViewBuilder private var loginToggle: some View {
        let login = app.loginItem
        switch login.state {
        case .unavailable(let why):
            Toggle(L("Start RustGit at login"), isOn: .constant(false)).disabled(true)
            Text(why).font(.caption).foregroundStyle(.secondary)
        case .needsApproval:
            Toggle(L("Start RustGit at login"), isOn: .constant(true)).disabled(true)
            Text(L("Approve RustGit in System Settings → General → Login Items.")).font(.caption).foregroundStyle(.orange)
        case .on, .off:
            Toggle(L("Start RustGit at login"), isOn: Binding(get: { login.state == .on }, set: { login.set($0) }))
            if let e = login.lastError { Text(e).font(.caption).foregroundStyle(.red) }
        }
    }
}
