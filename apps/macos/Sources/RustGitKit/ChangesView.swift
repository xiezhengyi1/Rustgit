import SwiftUI
import RgitClient

struct ChangesView: View {
    let project: ProjectModel
    private var message: String { project.commitMessageDraft }
    @State private var showConflicts = false

    var body: some View {
        VStack(spacing: 0) {
            ErrorBanner(project: project)
            if project.isMerging { mergeBanner }
            HSplitView {
                ChangeList(project: project).frame(minWidth: 270, idealWidth: 310, maxWidth: 330)
                DiffView(diff: project.focusDiff, state: project.diffState).frame(minWidth: 300)
            }
            commitBox
        }
        .sheet(isPresented: $showConflicts) { ConflictsSheet(project: project) }
        .task {
            // Show the first file's diff right away unless the project is so big that choosing is the user's job.
            if project.focus == nil, project.changeCount < 2000, let first = project.status?.changes.first { project.setFocus(first.path) }
        }
    }

    private var mergeBanner: some View {
        HStack {
            Image(systemName: "arrow.triangle.merge").foregroundStyle(.orange)
            VStack(alignment: .leading) {
                Text(LF("Merging %@", project.status?.merge?.label ?? "")).font(.headline)
                let open = project.status?.conflicts.filter { !$0.resolved }.count ?? 0
                Text(open > 0 ? LF("%d files still need a decision.", open) : L("All conflicts are decided. Commit to finish the merge."))
                    .font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            if project.hasConflicts { Button(L("Resolve conflicts…")) { showConflicts = true } }
            Button(L("Abort merge"), role: .destructive) { Task { await project.abortMerge() } }
        }
        .padding(10).background(.orange.opacity(0.12)).clipShape(RoundedRectangle(cornerRadius: 8))
        .padding(.horizontal, 12).padding(.top, 8)
    }

    private var commitBox: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(L("Save a version")).font(.system(size: 15, weight: .semibold))
                Text(project.hasRemote ? L("Commit the selected files and send this version to the host in one step.") : L("Describe what changed, then commit the selected files."))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            ZStack(alignment: .topLeading) {
                if message.isEmpty {
                    Text(L("For example: Fix login and update the settings page"))
                        .foregroundStyle(.tertiary).padding(.horizontal, 11).padding(.top, 10)
                        .allowsHitTesting(false)
                }
                TextEditor(text: Binding(get: { project.commitMessageDraft }, set: { project.commitMessageDraft = $0 }))
                    .scrollContentBackground(.hidden)
                    .padding(.horizontal, 6).padding(.vertical, 6)
                    .accessibilityLabel(L("Commit message"))
            }
            .font(.system(size: 13))
            .frame(height: 60)
            .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.primary.opacity(0.12)))
            if project.hasPendingShare && !project.commitAndShareInProgress {
                HStack {
                    Label(L("A local commit is waiting to be shared."), systemImage: "arrow.up.circle")
                        .font(.callout).foregroundStyle(.orange)
                    Spacer()
                    Button(L("Retry sharing")) { Task { await project.push() } }
                        .disabled(project.blocksChanges || project.isRunning(.push))
                }
            }
            HStack(spacing: 10) {
                Label(project.selectedCount == 0 ? L("Pick at least one file.")
                      : LF("%d of %d files selected", project.selectedCount, project.changeCount), systemImage: "checkmark.square")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                if project.isMerging && project.hasConflicts {
                    Text(L("Resolve all conflicts first.")).font(.callout).foregroundStyle(.orange)
                }
                if project.blocksChanges, let a = project.activities.first(where: { $0.blocksChanges }) {
                    Text(LF("Waiting for: %@", a.title)).font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                Text("⌘↩").font(.system(size: 12)).foregroundStyle(.tertiary).accessibilityHidden(true)
                Button { commit() } label: {
                    Text(project.hasRemote ? L("Commit and share") : L("Commit version")).font(.system(size: 13, weight: .semibold)).padding(.horizontal, 10)
                }
                .keyboardShortcut(.return, modifiers: .command)
                .buttonStyle(.borderedProminent).controlSize(.large)
                .disabled(!canCommit)
                .accessibilityHint(project.hasRemote ? L("Saves the selected files and sends the commit to the host") : L("Saves the selected files as a new version"))
            }
        }
        .padding(.horizontal, 20).padding(.vertical, 16)
        .background(Color(nsColor: .windowBackgroundColor))
        .overlay(alignment: .top) { Divider() }
    }

    /// Only things that really conflict with a commit disable it: another change to the working folder, an
    /// empty message or selection, unresolved conflicts. Background fetches, pushes, verifies and refreshes do not.
    private var canCommit: Bool {
        if project.blocksChanges || project.commitAndShareInProgress || (project.hasRemote && project.isRunning(.push)) || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return false }
        if project.isMerging { return !(project.status?.conflicts.contains { !$0.resolved } ?? false) }
        return project.selectedCount > 0
    }

    private func commit() {
        let msg = message
        Task {
            let saved = project.hasRemote ? await project.commitAndShare(message: msg) : await project.commit(message: msg)
            if saved, project.commitMessageDraft == msg { project.commitMessageDraft = "" }
        }
    }
}

/// Selection remains global while searching; filtering only changes which rows are visible.
struct ChangeList: View {
    let project: ProjectModel
    @State private var query = ""

    private var changes: [Change] {
        let all = project.status?.changes ?? []
        let term = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return term.isEmpty ? all : all.filter { $0.path.localizedStandardContains(term) }
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    Text(L("Changed files")).font(.system(size: 15, weight: .semibold))
                    Text(project.changeCount.formatted()).font(.system(size: 11, weight: .medium).monospacedDigit())
                        .padding(.horizontal, 7).padding(.vertical, 3)
                        .background(.primary.opacity(0.06), in: Capsule()).foregroundStyle(.secondary)
                    Spacer()
                    Button { Task { await project.refresh() } } label: { Image(systemName: "arrow.clockwise") }
                        .buttonStyle(.borderless).foregroundStyle(.secondary)
                        .help(L("Refresh changes")).accessibilityLabel(L("Refresh changes"))
                }
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.tertiary)
                    TextField(L("Filter files…"), text: $query).textFieldStyle(.plain)
                        .accessibilityLabel(L("Filter files…"))
                    if !query.isEmpty {
                        Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                            .buttonStyle(.plain).foregroundStyle(.secondary).accessibilityLabel(L("Clear filter"))
                    }
                }
                .font(.system(size: 12)).padding(.horizontal, 9).padding(.vertical, 7)
                .background(.primary.opacity(0.045), in: RoundedRectangle(cornerRadius: 7))
                HStack {
                    Toggle(L("Select all"), isOn: Binding(get: { project.allSelected }, set: { $0 ? project.selectAll() : project.selectNone() }))
                        .toggleStyle(.checkbox).disabled(project.changeCount == 0)
                        .accessibilityLabel(L("Select all changed files"))
                    Spacer()
                    Text(LF("%d selected", project.selectedCount)).foregroundStyle(.secondary).monospacedDigit()
                }.font(.system(size: 12))
            }
            .padding(16)
            Divider()
            if project.changeCount == 0 {
                ContentUnavailable(title: L("No changes"), systemImage: "checkmark.circle",
                                   text: L("Everything is committed. Edit a file in this folder and it will show up here."))
            } else if changes.isEmpty {
                ContentUnavailable(title: L("No matching files"), systemImage: "magnifyingglass",
                                   text: L("Try another file name or folder."))
            } else {
                List(changes, selection: Binding(get: { project.focus }, set: { project.setFocus($0) })) { c in
                    ChangeRow(project: project, change: c).tag(c.path)
                        .listRowSeparator(.hidden)
                        .listRowInsets(EdgeInsets(top: 6, leading: 10, bottom: 6, trailing: 10))
                }
                .listStyle(.inset).scrollContentBackground(.hidden)
            }
            if !project.skipped.isEmpty {
                Divider()
                DisclosureGroup(LF("%d files skipped", project.skipped.count)) {
                    List(project.skipped, id: \.path) { s in
                        VStack(alignment: .leading) { Text(s.path).font(.caption); Text(s.reason).font(.caption2).foregroundStyle(.secondary) }
                    }.frame(maxHeight: 160)
                }.padding(8).font(.callout)
            }
        }
        .background(Color(nsColor: .controlBackgroundColor))
    }
}

struct ChangeRow: View {
    let project: ProjectModel
    let change: Change

    var body: some View {
        HStack(spacing: 10) {
            Toggle("", isOn: Binding(get: { project.isSelected(change.path) }, set: { project.setSelected(change.path, $0) }))
                .toggleStyle(.checkbox).labelsHidden()
                .accessibilityLabel(LF("Include %@ in the commit", change.path))
            VStack(alignment: .leading, spacing: 4) {
                Text((change.path as NSString).lastPathComponent)
                    .font(.system(size: 13, weight: .medium)).foregroundStyle(.primary)
                    .lineLimit(1).truncationMode(.middle)
                let directory = (change.path as NSString).deletingLastPathComponent
                Text(directory.isEmpty ? L("Project root") : directory)
                    .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                if let from = change.from {
                    Text(LF("renamed from %@", from)).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(Self.symbol(change.kind))
                .font(.system(size: 10, weight: .bold, design: .monospaced))
                .foregroundStyle(Self.tint(change.kind))
                .frame(width: 21, height: 21)
                .background(Self.tint(change.kind).opacity(0.10), in: RoundedRectangle(cornerRadius: 5))
                .accessibilityHidden(true)
        }
        .padding(.vertical, 2)
        .help("\(Self.name(change.kind)): \(change.path)")
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(Self.name(change.kind)): \(change.path)")
    }

    static func name(_ k: ChangeKind) -> String {
        switch k {
        case .added: L("Added")
        case .modified: L("Modified")
        case .deleted: L("Deleted")
        case .renamed: L("Renamed")
        case .unknown(let s): s
        }
    }

    private static func symbol(_ k: ChangeKind) -> String {
        switch k { case .added: "A"; case .modified: "M"; case .deleted: "D"; case .renamed: "R"; case .unknown: "?" }
    }

    private static func tint(_ k: ChangeKind) -> Color {
        switch k { case .added: .green; case .modified: .orange; case .deleted: .red; case .renamed: .blue; case .unknown: .secondary }
    }

    @ViewBuilder static func icon(_ k: ChangeKind) -> some View {
        switch k {
        case .added: Image(systemName: "plus.circle.fill").foregroundStyle(.green)
        case .modified: Image(systemName: "pencil.circle.fill").foregroundStyle(.orange)
        case .deleted: Image(systemName: "minus.circle.fill").foregroundStyle(.red)
        case .renamed: Image(systemName: "arrow.right.circle.fill").foregroundStyle(.blue)
        case .unknown: Image(systemName: "questionmark.circle")
        }
    }
}

/// Per-file merge conflict resolution: keep ours, take theirs, or edit by hand.
struct ConflictsSheet: View {
    let project: ProjectModel
    @Environment(\.dismiss) private var dismiss
    @State private var editing: ConflictEntry?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("Resolve conflicts")).font(.title2.bold())
            Text(L("Both sides changed these files. Ours is what you have now; theirs comes from the branch being merged."))
                .foregroundStyle(.secondary)
            List(project.status?.conflicts ?? []) { c in
                HStack {
                    Image(systemName: c.resolved ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .foregroundStyle(c.resolved ? .green : .orange)
                    VStack(alignment: .leading) {
                        Text(c.path)
                        Text(c.resolved ? L("Decided") : LF("Conflict: %@", c.kind)).font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button(L("Keep ours")) { Task { await project.resolve(c.path, .ours) } }
                    Button(L("Take theirs")) { Task { await project.resolve(c.path, .theirs) } }
                    Button(L("Edit…")) { editing = c }
                }
                .disabled(project.activity != nil)
                .accessibilityElement(children: .contain)
            }
            .frame(minHeight: 200)
            if let e = project.error { banner(e, dismiss: { project.dismissError() }) }
            HStack {
                Button(L("Abort merge"), role: .destructive) { Task { await project.abortMerge(); dismiss() } }
                Spacer()
                Button(L("Done")) { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(20).frame(minWidth: 640, minHeight: 380)
        .sheet(item: $editing) { c in ConflictEditor(project: project, conflict: c) }
    }
}

struct ConflictEditor: View {
    let project: ProjectModel
    let conflict: ConflictEntry
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var binary = false
    @State private var loadError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(conflict.path).font(.title3.bold())
            if binary {
                Text(L("This file is not text, so it cannot be edited here. Keep ours or take theirs.")).foregroundStyle(.secondary)
            } else {
                Text(L("Edit the file so it contains exactly what you want, then mark it resolved."))
                    .foregroundStyle(.secondary)
                TextEditor(text: $text).font(.system(.body, design: .monospaced))
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
                    .accessibilityLabel(LF("Contents of %@", conflict.path))
            }
            if let loadError { Text(loadError).foregroundStyle(.red) }
            HStack {
                Button(L("Cancel")) { dismiss() }
                Spacer()
                Button(L("Save and mark resolved")) {
                    Task {
                        do {
                            try text.write(to: project.url.appendingPathComponent(conflict.path), atomically: true, encoding: .utf8)
                            await project.resolve(conflict.path, .workingTree)
                            dismiss()
                        } catch { loadError = error.localizedDescription }
                    }
                }
                .buttonStyle(.borderedProminent).disabled(binary)
            }
        }
        .padding(20).frame(minWidth: 640, minHeight: 440)
        .task {
            let file = project.url.appendingPathComponent(conflict.path)
            if let data = try? Data(contentsOf: file), let s = String(data: data, encoding: .utf8) { text = s }
            else if FileManager.default.fileExists(atPath: file.path) { binary = true }
            else { loadError = L("The file does not exist in your folder. Keep ours or take theirs.") ; binary = true }
        }
    }
}
