import Foundation
import RgitClient

/// Lane assignment for a compact commit graph (newest first, as `rgit log` prints).
public struct GraphRow: Equatable, Sendable {
    public var commit: String
    public var lane: Int
    /// Lanes that continue through this row (drawn as vertical lines).
    public var passing: [Int]
    /// Lanes of this commit's parents (edges leaving downward from the dot).
    public var parentLanes: [Int]
    public var laneCount: Int
}

public enum GraphLayout {
    public static func rows(for commits: [CommitInfo]) -> [GraphRow] {
        var lanes: [String?] = []     // lane i is waiting for this commit id
        var rows: [GraphRow] = []
        for c in commits {
            var lane = lanes.firstIndex(of: c.id)
            if lane == nil {
                lane = lanes.firstIndex(of: nil) ?? { lanes.append(nil); return lanes.count - 1 }()
            }
            let mine = lane!
            // Other lanes also waiting for this commit end here (merge of lines).
            for i in lanes.indices where lanes[i] == c.id && i != mine { lanes[i] = nil }
            let passing = lanes.indices.filter { lanes[$0] != nil && $0 != mine }
            var parentLanes: [Int] = []
            lanes[mine] = nil
            for (n, p) in c.parents.enumerated() {
                if let existing = lanes.firstIndex(of: p) {
                    parentLanes.append(existing)
                } else if n == 0 {
                    lanes[mine] = p
                    parentLanes.append(mine)
                } else {
                    let free = lanes.firstIndex(of: nil) ?? { lanes.append(nil); return lanes.count - 1 }()
                    lanes[free] = p
                    parentLanes.append(free)
                }
            }
            rows.append(GraphRow(commit: c.id, lane: mine, passing: passing, parentLanes: parentLanes, laneCount: lanes.count))
            while lanes.last == .some(nil) { lanes.removeLast() }
        }
        let width = rows.map(\.laneCount).max() ?? 1
        return rows.map { var r = $0; r.laneCount = max(width, r.lane + 1); return r }
    }
}
