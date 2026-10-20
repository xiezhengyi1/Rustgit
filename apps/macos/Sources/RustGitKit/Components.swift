import SwiftUI
import AppKit
import RgitClient

/// Always-visible strip: branch, server state, sync progress.
struct StatusBar: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        HStack(spacing: 14) {
            if let p = app.project {
                Label(p.branchName, systemImage: "arrow.triangle.branch")
                    .font(.callout.weight(.medium))
                    .accessibilityLabel(LF("Current branch: %@", p.branchName))
                if p.isMerging {
                    Label(L("Merge in progress"), systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange).font(.callout)
                }
                Divider().frame(height: 14)
                SyncSummary(project: p)
            } else {
                Text(L("No project open")).foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            ServerSummary()
            if let p = app.project {
                if p.isRefreshing && p.activities.isEmpty {
                    ProgressView().controlSize(.small).accessibilityLabel(L("Refreshing"))
                }
                ForEach(p.activities) { a in
                    Divider().frame(height: 14)
                    ActivityView(activity: a, onCancel: { p.cancelActivity(a.id) })
                }
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 7)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}

struct SyncSummary: View {
    let project: ProjectModel
    var body: some View {
        HStack(spacing: 6) {
            if !project.hasRemote {
                Label(L("Not shared"), systemImage: "icloud.slash").foregroundStyle(.secondary)
                    .accessibilityLabel(L("Sync state: not shared with any host"))
            } else {
                switch project.syncState {
                case .upToDate?: Label(L("In sync with host"), systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                case .behind?: Label(L("Host has newer work"), systemImage: "arrow.down.circle.fill").foregroundStyle(.blue)
                case .ahead?: Label(L("You have work to share"), systemImage: "arrow.up.circle.fill").foregroundStyle(.blue)
                case .diverged?: Label(L("You and the host both changed"), systemImage: "arrow.triangle.merge").foregroundStyle(.orange)
                case .noLocalCommits?: Label(L("Nothing committed yet"), systemImage: "circle.dashed").foregroundStyle(.secondary)
                case .unknown?, nil: Label(L("Not checked yet"), systemImage: "questionmark.circle").foregroundStyle(.secondary)
                }
            }
        }
        .font(.callout)
        .accessibilityElement(children: .combine)
    }
}

struct ServerSummary: View {
    @Environment(AppModel.self) private var app
    var body: some View {
        let hosts = app.hosts!
        let running = hosts.entries.filter { if case .running = hosts.state($0.id) { true } else { false } }
        let shown = running.first.flatMap { hosts.joinAddresses($0.id).last?.url.replacingOccurrences(of: "tcp://", with: "") } ?? ""
        HStack(spacing: 6) {
            Circle().fill(running.isEmpty ? Color.secondary.opacity(0.5) : Color.green).frame(width: 8, height: 8)
            if running.isEmpty {
                Text(hosts.entries.isEmpty ? L("Not hosting") : L("Hosting paused")).foregroundStyle(.secondary)
            } else {
                Text(running.count == 1 ? LF("Hosting on %@", shown) : LF("Hosting %d projects", running.count))
            }
        }
        .font(.callout)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(running.isEmpty ? L("Server: not running") : L("Server: running"))
    }
}

struct ActivityView: View {
    let activity: ActivityState
    let onCancel: () -> Void
    var body: some View {
        HStack(spacing: 8) {
            if let f = activity.fraction {
                ProgressView(value: f).frame(width: 90)
            } else {
                ProgressView().controlSize(.small)
            }
            Text(activity.phase.map { "\(activity.title) \($0)" } ?? activity.title).font(.callout).lineLimit(1)
            if activity.cancellable {
                Button(L("Cancel"), action: onCancel).controlSize(.small)
                    .accessibilityHint(L("Stops the transfer. It resumes where it stopped next time."))
            }
        }
        .accessibilityElement(children: .contain)
    }
}

struct ErrorBanner: View {
    let project: ProjectModel
    var body: some View {
        if let e = project.error {
            banner(e, dismiss: { project.dismissError() }, action: project.errorAction)
        } else if let m = project.lastMessage {
            HStack {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                Text(m)
                Spacer()
                Button { project.lastMessage = nil } label: { Image(systemName: "xmark") }
                    .buttonStyle(.borderless).accessibilityLabel(L("Dismiss"))
            }
            .padding(10).background(.green.opacity(0.12)).clipShape(RoundedRectangle(cornerRadius: 8))
            .padding(.horizontal, 12).padding(.top, 8)
            .accessibilityElement(children: .combine)
        }
    }
}

@MainActor
func banner(_ e: UserFacingError, dismiss: @escaping () -> Void, action: (title: String, run: @MainActor () async -> Void)? = nil) -> some View {
    VStack(alignment: .leading, spacing: 6) {
        HStack(alignment: .top) {
            Image(systemName: "exclamationmark.octagon.fill").foregroundStyle(.red)
            VStack(alignment: .leading, spacing: 3) {
                Text(e.title).font(.headline)
                if !e.suggestion.isEmpty { Text(e.suggestion) }
                if !e.paths.isEmpty {
                    Text(e.paths.prefix(8).joined(separator: "\n")).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary)
                    if e.paths.count > 8 { Text(LF("…and %d more", e.paths.count - 8)).font(.caption).foregroundStyle(.secondary) }
                }
                if !e.detail.isEmpty {
                    DisclosureGroup(L("Details")) {
                        Text(e.detail).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }.font(.caption)
                }
            }
            Spacer()
            Button(action: dismiss) { Image(systemName: "xmark") }
                .buttonStyle(.borderless).accessibilityLabel(L("Dismiss"))
        }
        if let action {
            Button(action.title) { Task { dismiss(); await action.run() } }.buttonStyle(.borderedProminent).controlSize(.small)
        }
    }
    .padding(10)
    .background(.red.opacity(0.10)).clipShape(RoundedRectangle(cornerRadius: 8))
    .padding(.horizontal, 12).padding(.top, 8)
    .accessibilityElement(children: .contain)
}

struct DiffView: View {
    let diff: FileDiff?
    var state: DiffState = .idle

    var body: some View {
        Group {
            switch state {
            case .loading(let slow):
                VStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(slow ? L("Computing the diff of a large project. This can take several seconds.") : L("Loading changes…"))
                        .font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }.padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed(let why):
                ContentUnavailable(title: L("The diff could not be loaded"), systemImage: "exclamationmark.triangle", text: why)
            case .idle:
                if let diff {
                    DiffDocumentView(diff: diff).id(diff.path)
                } else {
                    ContentUnavailable(title: L("No text changes to show"), systemImage: "doc.text.magnifyingglass",
                                       text: L("Select a file to see what changed."))
                }
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
    }
}

private struct DiffDocumentView: View {
    let diff: FileDiff
    private let rows: [NumberedDiffLine]
    private let contentWidth: CGFloat
    private let cap = 4000

    init(diff: FileDiff) {
        self.diff = diff
        self.rows = NumberedDiffLine.make(diff.lines)
        let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        // A lazy stack's flexible frame otherwise reports only the viewport width, even when a
        // fixed-size code line extends beyond it. Give the scroll view the actual document width.
        self.contentWidth = self.rows.prefix(4000).reduce(CGFloat(0)) { width, row in
            max(width, (row.text as NSString).size(withAttributes: [.font: font]).width + 152)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                Image(systemName: diff.isBinary ? "doc.zipper" : "doc.text")
                    .font(.system(size: 19)).foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 4) {
                    Text((diff.path as NSString).lastPathComponent)
                        .font(.system(size: 14, weight: .semibold)).lineLimit(1).truncationMode(.middle)
                    let directory = (diff.path as NSString).deletingLastPathComponent
                    Text(directory.isEmpty ? L("Project root") : directory)
                        .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }.frame(maxWidth: .infinity, alignment: .leading)
                if !diff.isBinary {
                    VStack(alignment: .trailing, spacing: 4) {
                        Text(L("Changes preview")).font(.system(size: 11)).foregroundStyle(.secondary)
                        HStack(spacing: 8) {
                            Text("+\(rows.filter { $0.kind == .added }.count)").foregroundStyle(.green)
                            Text("−\(rows.filter { $0.kind == .removed }.count)").foregroundStyle(.red)
                        }.font(.system(size: 12, weight: .semibold, design: .monospaced))
                    }.fixedSize()
                }
            }
            .padding(.horizontal, 20).padding(.vertical, 16)
            .background(Color(nsColor: .controlBackgroundColor))
            Divider()
            if diff.isBinary {
                ContentUnavailable(title: L("File preview unavailable"), systemImage: "doc.zipper",
                                   text: diff.lines.first?.text ?? L("This file cannot be displayed as text."))
            } else {
                GeometryReader { geometry in
                    ScrollView([.vertical, .horizontal]) {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(rows.prefix(cap).enumerated()), id: \.offset) { _, row in
                                DiffLineRow(row: row)
                            }
                            if rows.count > cap {
                                Text(LF("Showing the first %d lines of %d.", cap, rows.count))
                                    .font(.callout).foregroundStyle(.secondary).padding(16)
                            }
                        }
                        .frame(width: max(geometry.size.width, contentWidth), alignment: .leading)
                        .padding(.bottom, 16)
                        .textSelection(.enabled)
                    }
                }
            }
        }
        .accessibilityLabel(LF("Changes in %@", diff.path))
    }
}

private struct DiffLineRow: View {
    let row: NumberedDiffLine

    var body: some View {
        HStack(spacing: 0) {
            if row.kind == .hunk || row.kind == .meta {
                Text(row.text).foregroundStyle(.secondary)
                    .font(.system(size: 11, design: .monospaced))
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(.horizontal, 18)
                    .frame(height: 30)
            } else {
                HStack(spacing: 0) {
                    number(row.old)
                    number(row.new)
                }
                .background(.primary.opacity(0.025))
                Text(row.kind == .added ? "+" : row.kind == .removed ? "−" : " ")
                    .foregroundStyle(row.kind == .added ? Color.green : row.kind == .removed ? Color.red : Color.secondary)
                    .frame(width: 24)
                Text(row.text.isEmpty ? " " : row.text)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(.trailing, 24)
            }
        }
        .font(.system(size: 12, design: .monospaced))
        .frame(height: row.kind == .hunk ? 30 : 24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(background)
    }

    private func number(_ value: Int?) -> some View {
        Text(value.map(String.init) ?? "")
            .font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary.opacity(0.8))
            .frame(width: 44, height: 24, alignment: .trailing).padding(.trailing, 8)
    }

    private var background: Color {
        switch row.kind {
        case .added: .green.opacity(0.08)
        case .removed: .red.opacity(0.07)
        case .hunk: .blue.opacity(0.055)
        default: .clear
        }
    }
}

struct ContentUnavailable: View {
    let title: String
    let systemImage: String
    var text: String? = nil
    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: systemImage).font(.system(size: 34)).foregroundStyle(.secondary).accessibilityHidden(true)
            Text(title).font(.headline)
            if let text { Text(text).foregroundStyle(.secondary).multilineTextAlignment(.center) }
        }
        .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}

struct Card<Content: View>: View {
    let title: String
    var systemImage: String? = nil
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(title, systemImage: systemImage ?? "square").labelStyle(CardTitleStyle(hasIcon: systemImage != nil)).font(.headline)
            content
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5)).clipShape(RoundedRectangle(cornerRadius: 10))
    }
}
struct CardTitleStyle: LabelStyle {
    let hasIcon: Bool
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) { if hasIcon { configuration.icon }; configuration.title }
    }
}

/// A placeholder for a feature the engine does not implement yet. Never interactive.
struct UnavailableRow: View {
    let title: String
    let detail: String
    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "lock.fill").foregroundStyle(.secondary).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(title)
                    Text(L("Not available yet")).font(.caption2.weight(.semibold)).padding(.horizontal, 6).padding(.vertical, 2)
                        .background(.secondary.opacity(0.2)).clipShape(Capsule())
                }
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
        }
        .opacity(0.8)
        .accessibilityElement(children: .combine)
        .accessibilityHint(L("This feature is not built into the engine yet."))
    }
}

extension Date {
    var relativeText: String {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: self, relativeTo: Date())
    }
}
