import SwiftUI

struct StorageView: View {
    let project: ProjectModel
    @State private var aggressive = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                ErrorBanner(project: project)
                sizeCard
                maintenanceCard
                recoveryCard
            }.padding(14)
        }
        .task { await project.refreshStorage() }
    }

    private var sizeCard: some View {
        Card(title: L("Size and savings"), systemImage: "internaldrive") {
            if let s = project.stats {
                Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 6) {
                    row(L("Space used by history"), ByteFormat.string(s.repoBytes))
                    row(L("Size if every version were stored whole"), ByteFormat.string(s.logicalBytes))
                    row(L("Actually stored"), ByteFormat.string(s.storedBytes))
                    row(L("Savings"), s.dedupRatio.map { String(format: "%.1f×", $0) } ?? "—")
                    row(L("Versions (commits)"), "\(s.commits)")
                    row(L("Branches"), "\(s.branches)")
                    row(L("Stored pieces"), "\(s.objects)")
                    row(L("Pieces stored as differences"), "\(s.deltaObjects)")
                    row(L("Pack files"), "\(s.packFiles) (\(ByteFormat.string(s.packBytes)))")
                }
                .accessibilityElement(children: .contain)
            } else {
                ProgressView()
            }
        }
    }

    private func row(_ k: String, _ v: String) -> some View {
        GridRow { Text(k).foregroundStyle(.secondary); Text(v).monospacedDigit() }
            .accessibilityElement(children: .combine)
    }

    private var maintenanceCard: some View {
        Card(title: L("Check and maintain"), systemImage: "wrench.and.screwdriver") {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Button(L("Verify everything")) { Task { await project.verify() } }.disabled(project.isRunning(.verify))
                    Text(L("Re-reads every stored piece and checks it. Takes a while on large projects.")).font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: 240, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Button(L("Clean up storage")) { Task { await project.gc(aggressive: aggressive) } }.disabled(project.blocksChanges)
                    Toggle(L("Thorough (slow)"), isOn: $aggressive).font(.caption)
                    Text(L("Packs data tighter and removes old unreachable history after a grace period.")).font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: 240, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Button(L("Rebuild index")) { Task { await project.reindex() } }.disabled(project.blocksChanges)
                    Text(L("Fixes lookups if the index file was damaged. Never changes your history.")).font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: 240, alignment: .leading)
            }
            if let v = project.verifyResult {
                if v.problems.isEmpty {
                    Label(LF("All good: %d pieces and %d versions checked.", v.objectsChecked, v.commits), systemImage: "checkmark.seal.fill").foregroundStyle(.green)
                } else {
                    VStack(alignment: .leading) {
                        Label(LF("%d problems found.", v.problems.count), systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red)
                        ForEach(v.problems.prefix(10), id: \.self) { Text($0).font(.caption.monospaced()) }
                        Text(L("See Recovery below.")).font(.callout)
                    }
                }
            }
            if let g = project.gcResult {
                Label(LF("Cleanup done: %@ freed.", ByteFormat.string(g.freedBytes)), systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            }
            if let r = project.reindexResult {
                Label(r.problems.isEmpty ? LF("Index rebuilt from %d pack files (%d pieces).", r.packs, r.objects)
                                         : LF("Index rebuilt, but %d problems were found.", r.problems.count),
                      systemImage: r.problems.isEmpty ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                    .foregroundStyle(r.problems.isEmpty ? .green : .orange)
            }
        }
    }

    private var recoveryCard: some View {
        Card(title: L("Recovery"), systemImage: "lifepreserver") {
            Text(L("If something looks wrong:")).fontWeight(.medium)
            VStack(alignment: .leading, spacing: 4) {
                Text("1. " + L("Run “Verify everything”. It reports damaged or missing pieces without changing anything."))
                Text("2. " + L("If lookups fail but verify finds no damaged data, run “Rebuild index”."))
                Text("3. " + L("If pieces are really missing, fetch them again from a host that has the project: connect, then “Get updates”."))
                Text("4. " + L("Restores and reverts that would overwrite your edits keep a recovery copy, listed below."))
            }.font(.callout)
            if let s = project.shelves {
                Divider()
                Text(L("Saved-aside work and recovery copies")).fontWeight(.medium)
                if s.shelves.isEmpty && s.recovery.isEmpty { Text(L("None.")).foregroundStyle(.secondary) }
                ForEach(s.shelves + s.recovery) { sh in
                    HStack { Image(systemName: sh.ref.hasPrefix("shelf") ? "tray.full" : "lifepreserver"); Text(sh.ref).font(.callout.monospaced()); Text(sh.message ?? "").foregroundStyle(.secondary) }
                        .accessibilityElement(children: .combine)
                }
            }
        }
    }
}
