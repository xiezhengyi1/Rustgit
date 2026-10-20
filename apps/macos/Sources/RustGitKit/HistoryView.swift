import SwiftUI
import RgitClient

struct HistoryView: View {
    let project: ProjectModel
    @State private var selection: String?
    @State private var detail: ShowResult?
    @State private var detailFile: String?
    @State private var newBranchFrom: CommitInfo?
    @State private var newBranchName = ""
    @State private var switchToNew = true
    @State private var showNewBranch = false
    @State private var confirmDelete: String?
    @State private var confirmRevert: CommitInfo?
    @State private var showConflicts = false
    @State private var confirmRestore: CommitInfo?
    @State private var detailTask: Task<Void, Never>?
    @State private var detailProblem: String?

    private var selected: CommitInfo? { project.commits.first { $0.id == selection } }

    var body: some View {
        VStack(spacing: 0) {
            ErrorBanner(project: project)
            HSplitView {
                branchPane.frame(minWidth: 190, idealWidth: 220, maxWidth: 300)
                commitList.frame(minWidth: 320, idealWidth: 420)
                detailPane.frame(minWidth: 320)
            }
        }
        .onDisappear { detailTask?.cancel() }
        .onChange(of: selection) { _, id in loadDetail(id) }
        .onChange(of: project.status?.head) { _, _ in if selection == nil { selection = project.commits.first?.id } }
        .onAppear { if selection == nil { selection = project.commits.first?.id } }
        .sheet(isPresented: $showNewBranch) { newBranchSheet }
        .sheet(isPresented: $showConflicts) { ConflictsSheet(project: project) }
        .confirmationDialog(LF("Delete branch %@?", confirmDelete ?? ""), isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } }), titleVisibility: .visible) {
            Button(L("Delete branch"), role: .destructive) { if let b = confirmDelete { Task { await project.deleteBranch(b) } } }
        } message: { Text(L("Commits stay in history until storage cleanup removes unreachable ones.")) }
        .confirmationDialog(L("Restore files to this version?"), isPresented: Binding(get: { confirmRestore != nil }, set: { if !$0 { confirmRestore = nil } }), titleVisibility: .visible) {
            Button(L("Restore to this version")) {
                if let commit = confirmRestore { Task { await project.restoreVersion(commit) } }
            }
        } message: { Text(L("Restores tracked files in your folder. Your history and unrelated local files stay intact. Review the restored changes before committing. Unsaved edits will not be overwritten without a further choice.")) }
        .confirmationDialog(LF("Undo commit %@?", confirmRevert?.short ?? ""), isPresented: Binding(get: { confirmRevert != nil }, set: { if !$0 { confirmRevert = nil } }), titleVisibility: .visible) {
            Button(L("Undo this commit")) { if let c = confirmRevert { Task { await project.revert(c); if project.isMerging { showConflicts = true } } } }
        } message: { Text(L("Creates a new version that undoes this one. History is not rewritten.")) }
    }

    // MARK: Branches

    private var branchPane: some View {
        VStack(spacing: 0) {
            HStack {
                Text(L("Branches")).font(.headline)
                Spacer()
                Button { newBranchFrom = nil; newBranchName = ""; showNewBranch = true } label: { Image(systemName: "plus") }
                    .buttonStyle(.borderless).accessibilityLabel(L("New branch"))
            }.padding(10)
            Divider()
            List {
                ForEach(project.branches?.branches ?? []) { b in
                    let current = b.name == project.branches?.current
                    HStack {
                        Image(systemName: current ? "checkmark.circle.fill" : "circle").foregroundStyle(current ? Color.accentColor : .secondary)
                        VStack(alignment: .leading) {
                            Text(b.name).fontWeight(current ? .semibold : .regular)
                            Text(b.shortId).font(.caption.monospaced()).foregroundStyle(.secondary)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture(count: 2) { if !current { Task { await project.switchBranch(b.name) } } }
                    .contextMenu {
                        if !current {
                            Button(L("Switch to this branch")) { Task { await project.switchBranch(b.name) } }
                            Button(L("Merge into current branch")) { Task { if await project.merge(b.name) { showConflicts = true } } }
                            Divider()
                            Button(L("Delete…"), role: .destructive) { confirmDelete = b.name }
                        }
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(current ? LF("Branch %@, current", b.name) : LF("Branch %@", b.name))
                    .accessibilityAction(named: L("Switch to this branch")) { if !current { Task { await project.switchBranch(b.name) } } }
                    .accessibilityAction(named: L("Merge into current branch")) { if !current { Task { if await project.merge(b.name) { showConflicts = true } } } }
                    .accessibilityAction(named: L("Delete…")) { if !current { confirmDelete = b.name } }
                }
            }.listStyle(.inset)
            Divider()
            Text(L("Double-click to switch. Right-click for merge and delete."))
                .font(.caption).foregroundStyle(.secondary).padding(8)
        }
    }

    private var newBranchSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(newBranchFrom.map { LF("New branch from %@", $0.short) } ?? LF("New branch from %@", project.branchName)).font(.title3.bold())
            TextField(L("Branch name"), text: $newBranchName).textFieldStyle(.roundedBorder)
            Toggle(L("Switch to it now"), isOn: $switchToNew)
            HStack {
                Button(L("Cancel")) { showNewBranch = false }
                Spacer()
                Button(L("Create")) {
                    let name = newBranchName.trimmingCharacters(in: .whitespaces), from = newBranchFrom?.id
                    showNewBranch = false
                    Task { await project.createBranch(name, from: from, switchTo: switchToNew) }
                }
                .buttonStyle(.borderedProminent).disabled(newBranchName.trimmingCharacters(in: .whitespaces).isEmpty)
                .keyboardShortcut(.defaultAction)
            }
        }.padding(20).frame(width: 380)
    }

    // MARK: Commits

    private var commitList: some View {
        Group {
            if project.commits.isEmpty {
                ContentUnavailable(title: L("No versions yet"), systemImage: "clock.arrow.circlepath", text: L("Commit your first changes and they appear here."))
            } else {
                List(project.commits, selection: $selection) { c in
                    HStack(alignment: .center, spacing: 8) {
                        if let row = project.graph[c.id] { GraphGlyph(row: row).frame(width: CGFloat(max(row.laneCount, 1)) * 14, height: 34) }
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(c.subject.isEmpty ? L("(no description)") : c.subject).lineLimit(1)
                                ForEach(c.branches, id: \.self) { b in
                                    Text(b).font(.caption2.weight(.semibold)).padding(.horizontal, 5).padding(.vertical, 1)
                                        .background(b == project.branchName ? Color.accentColor.opacity(0.25) : Color.secondary.opacity(0.2))
                                        .clipShape(Capsule())
                                }
                            }
                            Text("\(c.authorName) · \(c.date.relativeText) · \(c.short)").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .tag(c.id)
                    .contextMenu {
                        Button(L("New branch from here…")) { newBranchFrom = c; newBranchName = ""; showNewBranch = true }
                        Button(L("Restore to this version…")) { confirmRestore = c }
                        Button(L("Undo this commit…")) { confirmRevert = c }.disabled(c.parents.isEmpty)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(c.subject), \(c.authorName), \(c.date.relativeText)")
                }.listStyle(.inset)
            }
        }
    }

    // MARK: Details

    private var detailPane: some View {
        Group {
            if let c = selected {
                VStack(alignment: .leading, spacing: 0) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(c.message).font(.headline).textSelection(.enabled)
                        Text("\(c.authorName) <\(c.authorEmail)>").font(.callout).foregroundStyle(.secondary)
                        Text(c.date.formatted(date: .abbreviated, time: .shortened) + " · " + c.id.prefix(10)).font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button(L("New branch…")) { newBranchFrom = c; newBranchName = ""; showNewBranch = true }
                            Button(L("Restore to this version…")) { confirmRestore = c }.buttonStyle(.borderedProminent).disabled(project.blocksChanges)
                            if let f = detailFile, detail?.changes.contains(where: { $0.path == f && $0.kind != .deleted }) == true {
                                Button(L("Restore this file")) { Task { await project.restore(paths: [f], from: c) } }
                                    .accessibilityHint(L("Puts this version of the file back in your folder"))
                            }
                        }.controlSize(.small).padding(.top, 4)
                    }.padding(10)
                    Divider()
                    if let d = detail {
                        HSplitView {
                            List(d.changes, selection: $detailFile) { ch in
                                HStack { Text(ch.kind == .added ? "+" : ch.kind == .deleted ? "−" : "~").monospaced().foregroundStyle(.secondary); Text(ch.path).lineLimit(1) }
                                    .tag(ch.path)
                            }.listStyle(.inset).frame(minWidth: 130, idealWidth: 170, maxWidth: 260)
                            DiffView(diff: detailFile.flatMap { DiffParser.section(of: d.diff, path: $0) })
                        }
                    } else {
                        VStack(spacing: 12) {
                            if let problem = detailProblem { Text(problem).foregroundStyle(.secondary) }
                            else {
                                ProgressView(L("Loading files in this version…"))
                                Button(L("Cancel preview")) { detailTask?.cancel(); detailProblem = L("Preview cancelled. You can still restore this version.") }
                            }
                        }.frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
            } else {
                ContentUnavailable(title: L("Select a version"), systemImage: "list.bullet.rectangle")
            }
        }
    }

    private func loadDetail(_ id: String?) {
        detailTask?.cancel()
        detail = nil; detailFile = nil; detailProblem = nil
        guard let id else { return }
        detailTask = Task {
            do {
                let result = try await project.client.show(id)
                guard !Task.isCancelled, selection == id else { return }
                detail = result; detailFile = result.changes.first?.path
            } catch {
                guard !Task.isCancelled, selection == id else { return }
                detailProblem = ErrorPresenter.present(error).title
            }
        }
    }

}

/// One row of the commit graph: lane lines, the dot, and edges to parents.
struct GraphGlyph: View {
    let row: GraphRow
    private let colors: [Color] = [.blue, .green, .orange, .purple, .pink, .teal]
    var body: some View {
        Canvas { ctx, size in
            let w: CGFloat = 14, mid = size.height / 2
            func x(_ lane: Int) -> CGFloat { CGFloat(lane) * w + w / 2 }
            for l in row.passing {
                var p = Path(); p.move(to: CGPoint(x: x(l), y: 0)); p.addLine(to: CGPoint(x: x(l), y: size.height))
                ctx.stroke(p, with: .color(colors[l % colors.count].opacity(0.7)), lineWidth: 1.5)
            }
            var up = Path(); up.move(to: CGPoint(x: x(row.lane), y: 0)); up.addLine(to: CGPoint(x: x(row.lane), y: mid))
            ctx.stroke(up, with: .color(colors[row.lane % colors.count].opacity(0.7)), lineWidth: 1.5)
            for pl in row.parentLanes {
                var p = Path(); p.move(to: CGPoint(x: x(row.lane), y: mid)); p.addLine(to: CGPoint(x: x(pl), y: size.height))
                ctx.stroke(p, with: .color(colors[pl % colors.count].opacity(0.7)), lineWidth: 1.5)
            }
            let dot = CGRect(x: x(row.lane) - 4, y: mid - 4, width: 8, height: 8)
            ctx.fill(Path(ellipseIn: dot), with: .color(colors[row.lane % colors.count]))
        }
        .accessibilityHidden(true)
    }
}
