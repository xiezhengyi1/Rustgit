import SwiftUI
import AppKit
import RgitClient

struct SharingView: View {
    let project: ProjectModel
    @Environment(AppModel.self) private var app
    @State private var access = SharingAccessModel()
    @State private var showInvite = false
    @State private var showJoin = false
    @State private var showAdvanced = false
    @State private var starting = false
    @State private var copied = false
    @State private var relay = ""
    @State private var newName = "origin"
    @State private var newURL = ""
    @State private var newHostKey = ""
    @State private var revokeDevice: EnrolledDevice?
    @State private var confirmLAN: UUID?
    @State private var confirmRemoveHost = false

    private var entry: HostedProject? { app.hosts.entry(forProject: project.url.path) }
    private var running: Bool {
        guard let entry else { return false }
        if case .running = app.hosts.state(entry.id) { return true }
        return false
    }
    private var committed: Bool { project.status?.head != nil }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                HStack {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(L("Share this project")).font(.system(size: 27, weight: .bold))
                        Text(project.name).font(.body).foregroundStyle(.secondary)
                    }
                    Spacer()
                    if running { Label(L("Sharing is on"), systemImage: "checkmark.shield.fill").foregroundStyle(.green) }
                }
                ErrorBanner(project: project)
                if let error = access.error { banner(error, dismiss: { access.error = nil }) }
                nextStep
                if entry != nil { inviteCard }
                if project.hasRemote { syncCard }
                if entry != nil { peopleCard }
                Button(L("Join someone else's project…")) { showJoin = true }.buttonStyle(.link)
                Divider()
                DisclosureGroup(L("Advanced connection settings"), isExpanded: $showAdvanced) {
                    if showAdvanced { advancedContent.padding(.top, 16) }
                }
            }
            .font(.system(size: 14))
            .frame(maxWidth: 900, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity)
        }
        .task(id: entry?.id) {
            access.invitation = nil
            await access.refresh(device: app.client, host: entry.flatMap { app.hosts.client(for: $0.id) })
        }
        .task(id: showAdvanced) {
            if showAdvanced, let client = app.client { await access.watch(client) }
        }
        .onDisappear { access.invitation = nil }
        .onChange(of: access.invitation) { _, _ in copied = false }
        .sheet(isPresented: $showInvite) {
            if let entry { InvitationSheet(entry: entry, access: access) }
        }
        .sheet(isPresented: $showJoin) { CloneSheet() }
        .confirmationDialog(L("Remove this hosted copy?"), isPresented: $confirmRemoveHost) {
            Button(L("Remove hosted copy…"), role: .destructive) {
                if let entry { Task { await app.hosts.remove(entry.id, deleteFiles: true) } }
            }
        } message: { Text(L("Hosting will stop and the hosted copy will be deleted. Your working folder stays intact. Get any shared updates you need before removing it.")) }
        .confirmationDialog(L("Revoke device access?"), isPresented: Binding(get: { revokeDevice != nil }, set: { if !$0 { revokeDevice = nil } })) {
            Button(L("Revoke access"), role: .destructive) {
                if let device = revokeDevice, let entry, let client = app.hosts.client(for: entry.id) {
                    Task { await access.perform { try await client.revokeDevice(device.fingerprint); access.devices = try await client.devices().devices } }
                }
            }
        } message: { Text(L("This ends future access and active sessions. Copies already downloaded remain on that device.")) }
        .confirmationDialog(L("Let other devices on this network connect?"), isPresented: Binding(get: { confirmLAN != nil }, set: { if !$0 { confirmLAN = nil } })) {
            Button(L("Allow other devices")) { if let id = confirmLAN { Task { await app.hosts.setAllowLAN(id, true) } } }
        } message: { Text(L("Connections are encrypted. Other devices need an invitation before they can access this project.")) }
    }

    private func panel<Content: View>(_ title: String, icon: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Label(title, systemImage: icon).font(.system(size: 18, weight: .semibold))
            content()
        }
        .padding(20).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(.primary.opacity(0.07)))
    }

    @ViewBuilder private var nextStep: some View {
        if !committed {
            panel(L("First, save a version to share"), icon: "1.circle.fill") {
                Text(L("Only committed versions are shared. Save your changes first, then come back to invite someone."))
                Button(L("Go to Changes and commit")) { app.section = .changes }
                    .buttonStyle(.borderedProminent).controlSize(.large)
            }
        } else if let entry {
            if !running {
                panel(L("Sharing is paused"), icon: "pause.circle") {
                    Text(L("Start hosting again so invited devices can connect to this Mac."))
                    Button(L("Resume sharing")) { Task { await app.hosts.start(entry.id) } }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                    if case .failed(let reason) = app.hosts.state(entry.id) { Text(reason).foregroundStyle(.red).textSelection(.enabled) }
                }
            }
        } else if !project.hasRemote {
            panel(L("Use this Mac to share the project"), icon: "desktopcomputer") {
                Text(L("Keep a shared copy on this Mac. You can then invite another computer to work on the project."))
                Button {
                    starting = true
                    Task {
                        await app.hostProject(project)
                        starting = false
                        if self.entry != nil, project.error == nil { showInvite = true }
                    }
                } label: {
                    HStack { if starting { ProgressView().controlSize(.small) }; Text(L("Set up sharing…")) }
                }.buttonStyle(.borderedProminent).controlSize(.large).disabled(starting || project.blocksChanges)
            }
        } else {
            panel(L("You have joined a shared project"), icon: "person.2.fill") {
                Text(L("Send your committed work and get other people's updates below. Ask the host owner to invite more people."))
            }
        }
    }

    private var inviteCard: some View {
        panel(L("Invite someone to collaborate"), icon: "person.badge.plus") {
            if access.invitation != nil {
                Text(L("Your invitation link is ready. Send it privately to the person you want to invite."))
                HStack {
                    Button(copied ? L("Copied. Send it to your collaborator.") : L("Copy invitation link")) { copyInvitation() }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                    Button(L("Create another invitation…")) { showInvite = true }.disabled(access.busy)
                }
            } else {
                Text(L("Create a link and send it to them. On their computer, open RustGit → Projects → Join a host, then paste the link."))
                Button(L("Create invitation link…")) { showInvite = true }
                    .buttonStyle(.borderedProminent).controlSize(.large)
                    .disabled(!committed || !running || access.busy)
            }
            if let entry {
                Label(entry.relay != nil ? L("Internet sharing configured") : (entry.allowLAN ? L("Available on the same local network") : L("Currently available only on this Mac")),
                      systemImage: entry.relay != nil ? "globe" : "wifi")
                    .foregroundStyle(.secondary)
            }
            Text(L("This Mac must stay awake with RustGit running so others can connect."))
                .foregroundStyle(.secondary)
        }
    }

    private var syncCard: some View {
        panel(L("Send and receive updates"), icon: "arrow.up.arrow.down") {
            SyncSummary(project: project).font(.body)
            Text(L("Send uploads your commits. Get updates downloads and merges the shared work into this folder."))
            HStack(spacing: 12) {
                Button(L("Send my commits")) { Task { await project.push() } }
                    .buttonStyle(.borderedProminent).controlSize(.large)
                    .disabled(!committed || project.isRunning(.push))
                Button(L("Get others' updates")) { Task { await project.pull() } }
                    .controlSize(.large).disabled(project.blocksChanges)
                Spacer()
                Button(L("Check for updates")) { Task { await project.fetch() } }.disabled(project.isRunning(.fetch))
            }
            if let date = project.lastFetch {
                Text(LF("Last checked at %@", date.formatted(date: .omitted, time: .shortened))).foregroundStyle(.secondary)
            }
            if let connection = project.connection {
                Label(connectionText(connection.state), systemImage: "network")
                if let reason = connection.error { Text(reason).foregroundStyle(.red).textSelection(.enabled) }
            }
        }
    }

    private var peopleCard: some View {
        panel(L("People with access"), icon: "person.2") {
            let guests = access.devices.filter { $0.status == "active" && $0.fingerprint != access.identity?.fingerprint }
            if guests.isEmpty {
                Text(L("Just you so far. Other devices appear here after they accept an invitation.")).foregroundStyle(.secondary)
            }
            ForEach(guests) { device in
                HStack {
                    Label(device.name.isEmpty ? L("Unnamed device") : device.name, systemImage: "laptopcomputer")
                    Spacer()
                    if let entry, let client = app.hosts.client(for: entry.id) {
                        Picker(L("Permission"), selection: Binding(get: { device.role }, set: { role in
                            Task { await access.perform { try await client.setDeviceRole(device.fingerprint, role: role); access.devices = try await client.devices().devices } }
                        })) { permissionOptions }
                        .labelsHidden().frame(width: 165).disabled(access.busy)
                        .accessibilityLabel(LF("Permission for %@", device.name))
                        Button(L("Remove access"), role: .destructive) { revokeDevice = device }.disabled(access.busy)
                    }
                }
            }
            if let entry, let client = app.hosts.client(for: entry.id) {
                DisclosureGroup(LF("Manage invitations (%d)", access.invitations.filter { $0.status == "active" }.count)) {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(access.invitations.filter { $0.status == "active" }) { invitation in
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(invitation.label.isEmpty ? L("Invitation link") : invitation.label)
                                    Text(roleText(invitation.role)).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Button(L("Disable invitation")) {
                                    Task { await access.perform { try await client.revokeInvitation(invitation.id); access.invitation = nil; access.invitations = try await client.invitations().invitations } }
                                }.disabled(access.busy)
                            }
                        }
                        Button(L("Refresh permissions")) { Task { await access.refresh(device: app.client, host: client) } }.disabled(access.busy)
                    }.padding(.top, 12)
                }
            }
        }
    }

    private var permissionOptions: some View {
        Group {
            Text(L("View only")).tag("read")
            Text(L("Can edit and submit")).tag("write")
            Text(L("Admin")).tag("admin")
        }
    }

    private func roleText(_ role: String) -> String {
        switch role { case "write": L("Can edit and submit"); case "admin": L("Admin"); default: L("View only") }
    }

    private func copyInvitation() {
        guard let invitation = access.invitation else { return }
        copy(invitation); copied = true
    }
    private func copy(_ text: String) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string) }
    private func connectionText(_ state: String) -> String {
        switch state {
        case "direct": L("Direct connection")
        case "relayed": L("Through a relay (works, may be slower)")
        case "unreachable": L("Cannot reach the host right now")
        default: L("Connecting…")
        }
    }

    private var advancedContent: some View {
        VStack(alignment: .leading, spacing: 20) {
            if let entry { hostSettings(entry) }
            connections
            DisclosureGroup(L("Nearby hosts")) {
                Text(L("Nearby addresses are unverified hints. Use an invitation or confirm the fingerprint with the host owner before connecting."))
                ForEach(access.nearby) { host in
                    ForEach(host.urls, id: \.self) { url in
                        HStack { Text(url).font(.body.monospaced()).textSelection(.enabled); Button(L("Use address")) { newURL = url } }
                    }
                }
            }
            if let identity = access.identity {
                DisclosureGroup(L("This device")) {
                    Text(identity.fingerprint).font(.body.monospaced()).textSelection(.enabled)
                    Text(identity.keyStorage)
                }
            }
        }
    }

    private func hostSettings(_ entry: HostedProject) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label(L("Hosting"), systemImage: "server.rack").font(.headline)
                Spacer()
                Button(running ? L("Stop hosting") : L("Start hosting")) { Task { if running { await app.hosts.stop(entry.id) } else { await app.hosts.start(entry.id) } } }
            }
            Toggle(L("Allow other devices on this network"), isOn: Binding(get: { entry.allowLAN }, set: { value in
                if value { confirmLAN = entry.id } else { Task { await app.hosts.setAllowLAN(entry.id, false) } }
            }))
            DisclosureGroup(L("Addresses and security")) {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(app.hosts.joinAddresses(entry.id)) { address in
                        HStack { Text(address.url).font(.body.monospaced()).textSelection(.enabled); Spacer(); Button(L("Copy")) { copy(address.url) } }
                    }
                    if let fingerprint = app.hosts.fingerprints[entry.id] {
                        Text(L("Host fingerprint")).fontWeight(.medium)
                        Text(fingerprint).font(.body.monospaced()).textSelection(.enabled)
                    }
                }.padding(.top, 10)
            }
            DisclosureGroup(L("Internet relay")) {
                VStack(alignment: .leading, spacing: 10) {
                    Text(L("For people on a different network, use a relay provided by your administrator."))
                    HStack {
                        TextField(L("Relay URL (optional)"), text: $relay).textFieldStyle(.roundedBorder)
                        Button(L("Apply relay")) { Task { await app.hosts.setRelay(entry.id, relay) } }
                    }.onAppear { relay = entry.relay ?? "" }
                    if let url = app.hosts.irohURLs[entry.id] { Text(url).font(.body.monospaced()).textSelection(.enabled) }
                }.padding(.top, 10)
            }
            Button(L("Remove hosted copy…"), role: .destructive) { confirmRemoveHost = true }
            if let log = app.hosts.activity[entry.id], !log.isEmpty {
                DisclosureGroup(L("Recent host activity")) {
                    ForEach(log.suffix(10)) { Text($0.text).font(.body.monospaced()).textSelection(.enabled) }
                }
            }
        }
    }

    private var connections: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("Connections")).font(.headline)
            ForEach(project.remotes) { remote in
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(remote.name).fontWeight(.medium)
                        Text(remote.url).font(.body.monospaced()).textSelection(.enabled)
                        Text(remote.secure == true ? L("Encrypted · host identity pinned") : L("Host identity is not pinned")).foregroundStyle(.secondary)
                        if let key = remote.hostKey { Text(key).font(.body.monospaced()).textSelection(.enabled) }
                    }
                    Spacer()
                    Button(L("Remove")) { Task { await project.removeRemote(remote.name) } }
                }
            }
            DisclosureGroup(L("Add a host manually")) {
                VStack(alignment: .leading, spacing: 10) {
                    TextField(L("Name"), text: $newName)
                    TextField(L("Host address, e.g. tcp://127.0.0.1:7878"), text: $newURL)
                    TextField(L("Pinned host fingerprint (required for a new device)"), text: $newHostKey)
                    Button(L("Add")) { Task { await project.addRemote(name: newName, url: newURL, hostKey: newHostKey.isEmpty ? nil : newHostKey) } }
                        .disabled(newName.isEmpty || newURL.isEmpty || project.blocksChanges)
                }.textFieldStyle(.roundedBorder).padding(.top, 10)
            }
        }
    }
}
