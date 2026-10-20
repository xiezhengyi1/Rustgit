import SwiftUI
import AppKit
import RgitClient

@MainActor
func chooseFolder(prompt: String, canCreate: Bool = true) -> URL? {
    let p = NSOpenPanel()
    p.canChooseFiles = false
    p.canChooseDirectories = true
    p.canCreateDirectories = canCreate
    p.allowsMultipleSelection = false
    p.prompt = prompt
    return p.runModal() == .OK ? p.url : nil
}

struct ProjectsView: View {
    @Environment(AppModel.self) private var app
    @State private var showNew = false
    @State private var showClone = false
    @State private var offerInit: URL?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let b = app.banner { banner(b, dismiss: { app.banner = nil }) }
                HStack(spacing: 12) {
                    bigButton(L("New project…"), "plus.square.on.square", hint: L("Start tracking a folder")) { showNew = true }
                    bigButton(L("Open folder…"), "folder", hint: L("Open a folder that is already a project")) { openFolder() }
                    bigButton(L("Join a host…"), "arrow.down.circle", hint: L("Copy a project from a host address")) { showClone = true }
                }
                Card(title: L("Recent projects"), systemImage: "clock") {
                    if app.settings.projects.isEmpty {
                        Text(L("Nothing yet. Create or open a project to begin.")).foregroundStyle(.secondary)
                    }
                    ForEach(app.settings.projects) { p in
                        HStack {
                            Image(systemName: "folder.fill").foregroundStyle(.blue)
                            VStack(alignment: .leading) { Text(p.name).fontWeight(.medium); Text(p.path).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
                            Spacer()
                            if app.project?.url.path == p.path { Text(L("Open")).font(.caption).foregroundStyle(.secondary) }
                            else { Button(L("Open")) { Task { await app.open(URL(fileURLWithPath: p.path)) } } }
                            Button { app.settings.forget(p.path) } label: { Image(systemName: "xmark") }
                                .buttonStyle(.borderless).accessibilityLabel(LF("Remove %@ from the list", p.name))
                                .help(L("Remove from this list (files are not touched)"))
                        }.accessibilityElement(children: .contain)
                    }
                }
                hostsCard
            }.padding(16)
        }
        .sheet(isPresented: $showNew) { NewProjectSheet() }
        .sheet(isPresented: Binding(get: { showClone || app.pendingInvitation != nil }, set: { showClone = $0; if !$0 { app.pendingInvitation = nil } })) { CloneSheet() }
        .alert(L("This folder is not a project yet"), isPresented: Binding(get: { offerInit != nil }, set: { if !$0 { offerInit = nil } })) {
            Button(L("Start tracking it")) { if let u = offerInit { Task { await app.create(at: u, branch: "main", userName: "", userEmail: "") } } }
            Button(L("Cancel"), role: .cancel) {}
        } message: { Text(L("Start tracking this folder with RustGit? Your files stay where they are.")) }
    }

    private var hostsCard: some View {
        let hosts = app.hosts!
        return Card(title: L("Projects hosted from this Mac"), systemImage: "server.rack") {
            if hosts.entries.isEmpty {
                Text(L("None. Open a project, then Sharing → Host this project.")).foregroundStyle(.secondary)
            }
            ForEach(hosts.entries) { e in
                HStack {
                    Circle().fill({ if case .running = hosts.state(e.id) { Color.green } else { Color.secondary } }()).frame(width: 9, height: 9)
                    VStack(alignment: .leading) { Text(e.name).fontWeight(.medium); Text(hosts.joinAddresses(e.id).last?.url ?? "tcp://\(e.listen)").font(.caption.monospaced()).foregroundStyle(.secondary) }
                    Spacer()
                    if case .running = hosts.state(e.id) { Button(L("Stop")) { Task { await hosts.stop(e.id) } } }
                    else { Button(L("Start")) { Task { await hosts.start(e.id) } } }
                }.accessibilityElement(children: .contain)
            }
        }
    }

    private func bigButton(_ title: String, _ icon: String, hint: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) { Image(systemName: icon).font(.system(size: 28)); Text(title) }
                .frame(maxWidth: .infinity, minHeight: 84)
        }
        .buttonStyle(.bordered).controlSize(.large).accessibilityHint(hint)
    }

    private func openFolder() {
        guard let url = chooseFolder(prompt: L("Open"), canCreate: false) else { return }
        Task { if await !app.open(url), app.banner?.code == .notARepository { app.banner = nil; offerInit = url } }
    }
}

struct NewProjectSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var folder: URL?
    @State private var branch = "main"
    @State private var name = ""
    @State private var email = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("New project")).font(.title2.bold())
            HStack {
                Text(folder?.path ?? L("No folder chosen")).lineLimit(1).truncationMode(.middle).foregroundStyle(folder == nil ? .secondary : .primary)
                Spacer()
                Button(L("Choose folder…")) { folder = chooseFolder(prompt: L("Choose")) }
            }
            Text(L("Existing files stay where they are. Make a new folder in the chooser to start empty.")).font(.callout).foregroundStyle(.secondary)
            TextField(L("First branch name"), text: $branch).textFieldStyle(.roundedBorder)
            TextField(L("Your name (shown on your versions)"), text: $name).textFieldStyle(.roundedBorder)
            TextField(L("Your email"), text: $email).textFieldStyle(.roundedBorder)
            HStack {
                Button(L("Cancel")) { dismiss() }
                Spacer()
                Button(L("Create")) {
                    guard let folder else { return }
                    dismiss()
                    Task { await app.create(at: folder, branch: branch, userName: name, userEmail: email) }
                }.buttonStyle(.borderedProminent).disabled(folder == nil || branch.isEmpty).keyboardShortcut(.defaultAction)
            }
        }.padding(20).frame(width: 460)
    }
}

struct CloneSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var address = ""
    @State private var hostKey = ""
    @State private var parent: URL?
    @State private var folderName = ""
    @State private var fraction: Double?
    @State private var phase: String?
    @State private var working = false
    @State private var task: Task<Void, Never>?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("Join a host")).font(.title2.bold())
            TextField(L("Invitation link or tcp://, ssh://, iroh:// address"), text: $address).textFieldStyle(.roundedBorder)
            if !address.hasPrefix("projectapp://") { TextField(L("Pinned host fingerprint (required for a new device)"), text: $hostKey).textFieldStyle(.roundedBorder) }
            HStack {
                Text(parent?.path ?? L("Where to put the project")).lineLimit(1).truncationMode(.middle).foregroundStyle(parent == nil ? .secondary : .primary)
                Spacer()
                Button(L("Choose folder…")) { parent = chooseFolder(prompt: L("Choose")) }
            }
            TextField(L("Folder name for the copy"), text: $folderName).textFieldStyle(.roundedBorder)
            Text(L("Paste an invitation to enroll this device. For a direct address, obtain the fingerprint and access from the host owner first.")).font(.callout).foregroundStyle(.secondary)
            if working {
                HStack {
                    if let f = fraction { ProgressView(value: f) } else { ProgressView().controlSize(.small) }
                    Text(phase ?? L("Connecting…")).font(.callout)
                }
            }
            if let b = app.banner { banner(b, dismiss: { app.banner = nil }) }
            HStack {
                Button(working ? L("Cancel download") : L("Close")) { if working { task?.cancel() } else { dismiss() } }
                Spacer()
                Button(L("Download")) {
                    guard let parent else { return }
                    let dest = parent.appendingPathComponent(folderName)
                    working = true; app.banner = nil
                    task = Task {
                        let ok = await app.clone(address: address, into: dest, hostKey: hostKey.isEmpty ? nil : hostKey) { e in
                            Task { @MainActor in fraction = e.fraction; phase = e.phase }
                        }
                        working = false
                        if ok { dismiss() }
                    }
                }.buttonStyle(.borderedProminent).disabled(working || parent == nil || folderName.isEmpty || address.count < 7)
                    .keyboardShortcut(.defaultAction)
            }
        }.padding(20).frame(width: 500)
        .onAppear { if let invitation = app.pendingInvitation { address = invitation } }
        .onDisappear { task?.cancel() }
    }
}
