import SwiftUI
import RgitClient

struct RootView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        Group {
            switch app.engine {
            case .checking:
                ProgressView(L("Starting the engine…")).frame(maxWidth: .infinity, maxHeight: .infinity)
            case .unavailable(let e):
                VStack(spacing: 12) {
                    Image(systemName: "exclamationmark.octagon.fill").font(.system(size: 44)).foregroundStyle(.red)
                    banner(e, dismiss: {})
                }.frame(maxWidth: 560).frame(maxWidth: .infinity, maxHeight: .infinity)
            case .ready:
                NavigationSplitView {
                    List(selection: Binding(get: { app.section }, set: { app.section = $0 ?? .projects })) {
                        Section(L("Recent projects")) {
                            ForEach(app.settings.projects) { known in
                                Button {
                                    Task { await app.open(URL(fileURLWithPath: known.path)) }
                                } label: {
                                    HStack(spacing: 8) {
                                        Image(systemName: app.project?.id == known.path ? "folder.fill" : "folder")
                                            .foregroundStyle(app.project?.id == known.path ? Color.accentColor : .secondary)
                                        Text(known.name).lineLimit(1).truncationMode(.middle)
                                        Spacer(minLength: 0)
                                        if app.openingProjectPath == known.path {
                                            ProgressView().controlSize(.mini)
                                        } else if app.project?.id == known.path {
                                            Image(systemName: "checkmark").font(.system(size: 10, weight: .semibold)).foregroundStyle(Color.accentColor)
                                        }
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .disabled(app.openingProjectPath != nil)
                                .help(known.path)
                                .accessibilityLabel(app.project?.id == known.path ? LF("Open project: %@", known.name) : LF("Switch to %@", known.name))
                                .contextMenu {
                                    Text(known.path)
                                    Button(L("Remove from recent projects")) { app.settings.forget(known.path) }
                                }
                            }
                        }
                        Section(L("Work")) {
                            ForEach(AppSection.allCases) { s in
                                Label(title(s), systemImage: icon(s)).tag(s)
                                    .disabled(s != .projects && app.project == nil)
                            }
                        }
                    }
                    .navigationSplitViewColumnWidth(min: 180, ideal: 200, max: 260)
                } detail: {
                    VStack(spacing: 0) {
                        if app.section != .projects, let error = app.banner {
                            banner(error, dismiss: { app.banner = nil })
                        }
                        detail
                        StatusBar()
                    }
                }
            }
        }
        .frame(minWidth: 960, minHeight: 600)
        .navigationTitle(app.project?.name ?? "RustGit")
    }

    @ViewBuilder private var detail: some View {
        if let p = app.project, app.section != .projects {
            Group { switch app.section {
            case .changes: ChangesView(project: p)
            case .history: HistoryView(project: p)
            case .sharing: SharingView(project: p).id(p.id)
            case .storage: StorageView(project: p)
            case .projects: EmptyView()
            } }.id(p.id)
        } else {
            ProjectsView()
        }
    }

    private func title(_ s: AppSection) -> String {
        switch s {
        case .projects: L("Projects")
        case .changes: L("Changes")
        case .history: L("History and branches")
        case .sharing: L("Sharing")
        case .storage: L("Storage and recovery")
        }
    }
    private func icon(_ s: AppSection) -> String {
        switch s {
        case .projects: "square.grid.2x2"
        case .changes: "pencil.and.list.clipboard"
        case .history: "clock.arrow.circlepath"
        case .sharing: "person.2.wave.2"
        case .storage: "internaldrive"
        }
    }
}
