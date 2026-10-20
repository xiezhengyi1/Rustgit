import SwiftUI
import AppKit
import RgitClient

/// Shareable invitations must never point another computer at its own loopback listener.
enum SharingInvitationHints {
    static func make(addresses: [String], internetURL: String?, viaInternet: Bool) -> [String] {
        if viaInternet { return internetURL.map { [$0] } ?? [] }
        return addresses.filter { address in
            guard let host = URL(string: address)?.host else { return false }
            return !host.hasPrefix("127.") && !["localhost", "::1", "[::1]", "0.0.0.0", "::", "[::]"].contains(host)
        }
    }
}

struct InvitationSheet: View {
    let entry: HostedProject
    let access: SharingAccessModel
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var role = "write"
    @State private var viaInternet = false
    @State private var relay = ""
    @State private var expiry = "24h"
    @State private var singleUse = true
    @State private var label = ""
    @State private var working = false
    @State private var created = false
    @State private var copied = false
    @State private var problem: String?
    @State private var task: Task<Void, Never>?

    private var needsLAN: Bool { !viaInternet && !(app.hosts.entries.first { $0.id == entry.id }?.allowLAN ?? false) }
    private var createTitle: String {
        if working { return L("Preparing your invitation…") }
        return needsLAN ? L("Allow local network and create link") : L("Create invitation link")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(created ? L("Your invitation is ready") : L("Invite a collaborator")).font(.system(size: 24, weight: .bold))
            if created { success } else { form }
            if let problem { Text(problem).foregroundStyle(.red).textSelection(.enabled).accessibilityLabel(problem) }
            Divider()
            HStack {
                Button(working ? L("Cancel") : L("Close")) { task?.cancel(); dismiss() }
                Spacer()
                if created {
                    Button(L("Done")) { dismiss() }.buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                } else {
                    Button(createTitle) {
                        task = Task { await create() }
                    }.buttonStyle(.borderedProminent).controlSize(.large)
                        .disabled(working || (viaInternet && relay.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
        .font(.system(size: 14)).padding(26).frame(width: 560)
        .onAppear { relay = entry.relay ?? ""; viaInternet = entry.relay != nil }
        .onDisappear { task?.cancel() }
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L("Choose how they will connect and what they can do. The link is valid for 24 hours and can be used once by default."))
            VStack(alignment: .leading, spacing: 8) {
                Text(L("Where is the other computer?")).fontWeight(.semibold)
                Picker(L("Network"), selection: $viaInternet) {
                    Text(L("Same Wi-Fi or local network")).tag(false)
                    Text(L("Different network")).tag(true)
                }.pickerStyle(.segmented).labelsHidden()
            }
            if viaInternet {
                Text(L("For people on a different network, use a relay provided by your administrator."))
                TextField(L("Relay URL"), text: $relay).textFieldStyle(.roundedBorder)
            } else if needsLAN {
                Text(L("Creating the link will allow devices on your local network to connect. Connections stay encrypted and require an invitation."))
                    .foregroundStyle(.secondary)
            }
            VStack(alignment: .leading, spacing: 8) {
                Text(L("What can they do?")).fontWeight(.semibold)
                Picker(L("Permission"), selection: $role) {
                    Text(L("Can edit and submit")).tag("write")
                    Text(L("View only")).tag("read")
                    Text(L("Admin")).tag("admin")
                }.labelsHidden().frame(maxWidth: .infinity, alignment: .leading)
                if role == "admin" { Text(L("Admins can overwrite shared history and delete remote branches.")).foregroundStyle(.orange) }
            }
            DisclosureGroup(L("More invitation options")) {
                VStack(alignment: .leading, spacing: 12) {
                    TextField(L("Invitation label"), text: $label).textFieldStyle(.roundedBorder)
                    Picker(L("Link expires after"), selection: $expiry) {
                        Text(L("24 hours")).tag("24h")
                        Text(L("7 days")).tag("7d")
                    }
                    Toggle(L("Single-use invitation"), isOn: $singleUse)
                }.padding(.top, 12)
            }
            if working { HStack { ProgressView().controlSize(.small); Text(L("Preparing your invitation…")) } }
        }.disabled(working)
    }

    private var success: some View {
        VStack(alignment: .leading, spacing: 18) {
            Label(L("Send this link privately. Anyone who has it can accept the invitation."), systemImage: "lock.fill")
            instruction("1", L("Copy the invitation link below."))
            instruction("2", L("Send it to your collaborator using your usual messenger."))
            instruction("3", L("On their computer: open RustGit → Projects → Join a host, and paste the link."))
            Button(copied ? L("Copied. Send it to your collaborator.") : L("Copy invitation link")) {
                guard let link = access.invitation else { return }
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(link, forType: .string)
                copied = true
            }.buttonStyle(.borderedProminent).controlSize(.large)
            DisclosureGroup(L("View invitation link")) {
                Text(access.invitation ?? "").font(.body.monospaced()).textSelection(.enabled).lineLimit(5)
            }
        }
    }

    private func instruction(_ number: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text(number).font(.headline).frame(width: 26, height: 26).background(Color.accentColor.opacity(0.12), in: Circle())
            Text(text).fixedSize(horizontal: false, vertical: true)
        }
    }

    private func create() async {
        working = true; problem = nil
        defer { working = false }
        do {
            if viaInternet {
                let address = relay.trimmingCharacters(in: .whitespacesAndNewlines)
                guard let url = URL(string: address), ["http", "https"].contains(url.scheme ?? ""), url.host != nil,
                      url.user == nil, url.password == nil else {
                    problem = L("Enter an HTTP or HTTPS relay URL without credentials."); return
                }
                if entry.relay != address { await app.hosts.setRelay(entry.id, address) }
                for _ in 0..<100 {
                    try Task.checkCancellation()
                    if app.hosts.irohURLs[entry.id] != nil { break }
                    try await Task.sleep(for: .milliseconds(200))
                }
            } else if needsLAN {
                await app.hosts.setAllowLAN(entry.id, true)
            }
            try Task.checkCancellation()
            guard case .running = app.hosts.state(entry.id) else {
                problem = L("Sharing could not start. Check the hosting status and try again."); return
            }
            let hints = SharingInvitationHints.make(addresses: app.hosts.joinAddresses(entry.id).map(\.url), internetURL: app.hosts.irohURLs[entry.id], viaInternet: viaInternet)
            guard !hints.isEmpty else {
                problem = viaInternet ? L("The internet address is not ready. Check the relay settings and try again.") : L("No local network address was found. Connect this Mac to Wi-Fi or Ethernet and try again.")
                return
            }
            guard let client = app.hosts.client(for: entry.id) else { return }
            let invitation = try await client.createInvitation(role: role, expires: expiry, singleUse: singleUse, label: label, hints: hints)
            access.invitation = invitation.uri
            created = true
            access.invitations = try await client.invitations().invitations
        } catch {
            if !Task.isCancelled {
                let shown = ErrorPresenter.present(error)
                problem = shown.title + "\n" + shown.suggestion
            }
        }
    }
}
