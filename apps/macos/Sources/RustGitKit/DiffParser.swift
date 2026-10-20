import Foundation

/// One file's slice of a unified diff as printed by `rgit diff` / `rgit show`.
public struct FileDiff: Identifiable, Equatable, Sendable {
    public enum LineKind: Sendable { case context, added, removed, hunk, meta }
    public struct Line: Equatable, Sendable { public var kind: LineKind; public var text: String }

    public var path: String
    public var isBinary: Bool
    public var lines: [Line]
    public var id: String { path }
}

/// Display rows retain the old/new coordinates separately from the diff markers.
/// Only file-header metadata is hidden; a removed line beginning with `---` stays visible.
struct NumberedDiffLine {
    var kind: FileDiff.LineKind
    var text: String
    var old: Int?
    var new: Int?

    static func make(_ lines: [FileDiff.Line]) -> [Self] {
        var old = 0, new = 0
        var rows: [Self] = []
        rows.reserveCapacity(lines.count)
        for line in lines {
            if line.kind == .meta && (line.text.hasPrefix("--- ") || line.text.hasPrefix("+++ ")) { continue }
            switch line.kind {
            case .hunk:
                let parts = line.text.split(separator: " ")
                func start(_ prefix: Character) -> Int {
                    guard let token = parts.first(where: { $0.first == prefix }) else { return 0 }
                    return Int(token.dropFirst().split(separator: ",").first ?? "0") ?? 0
                }
                old = start("-"); new = start("+")
                rows.append(Self(kind: .hunk, text: line.text))
            case .added:
                rows.append(Self(kind: .added, text: String(line.text.dropFirst()), new: new)); new += 1
            case .removed:
                rows.append(Self(kind: .removed, text: String(line.text.dropFirst()), old: old)); old += 1
            case .context:
                rows.append(Self(kind: .context, text: String(line.text.dropFirst()), old: old, new: new))
                old += 1; new += 1
            case .meta:
                rows.append(Self(kind: .meta, text: line.text))
            }
        }
        return rows
    }
}

public enum DiffParser {
    /// Splits the combined diff into per-file sections (all of them, or the first `maxFiles`).
    /// Hunk headers (`@@ -a,b +c,d @@`) give the exact number of body lines, so content such as `--- foo`
    /// inside a hunk is never mistaken for a file header. Lines are scanned lazily: stopping early costs
    /// only what was read.
    public static func parse(_ text: String, maxFiles: Int = .max) -> [FileDiff] {
        var scanner = LineScanner(text)
        var result: [FileDiff] = []
        while result.count < maxFiles, let line = scanner.next() {
            if line.hasPrefix("Binary files a/"), line.hasSuffix(" differ") {
                let body = line.dropFirst("Binary files a/".count).dropLast(" differ".count)
                let path = body.components(separatedBy: " and b/").last ?? String(body)
                result.append(FileDiff(path: path, isBinary: true, lines: [FileDiff.Line(kind: .meta, text: String(line))]))
            } else if line.hasPrefix("--- "), let plus = scanner.peek(), plus.hasPrefix("+++ ") {
                _ = scanner.next()
                let old = String(line.dropFirst(4)), new = String(plus.dropFirst(4))
                let path = new == "/dev/null" ? stripPrefix(old, "a/") : stripPrefix(new, "b/")
                var out: [FileDiff.Line] = [FileDiff.Line(kind: .meta, text: String(line)), FileDiff.Line(kind: .meta, text: String(plus))]
                while let h = scanner.peek(), h.hasPrefix("@@") {
                    _ = scanner.next()
                    out.append(FileDiff.Line(kind: .hunk, text: String(h)))
                    var (oldLeft, newLeft) = counts(String(h))
                    while oldLeft > 0 || newLeft > 0, let l = scanner.next() {
                        if l.hasPrefix("+") { out.append(FileDiff.Line(kind: .added, text: String(l))); newLeft -= 1 }
                        else if l.hasPrefix("-") { out.append(FileDiff.Line(kind: .removed, text: String(l))); oldLeft -= 1 }
                        else if l.hasPrefix("\\") { out.append(FileDiff.Line(kind: .meta, text: String(l))) }
                        else { out.append(FileDiff.Line(kind: .context, text: String(l))); oldLeft -= 1; newLeft -= 1 }
                    }
                    while let n = scanner.peek(), n.hasPrefix("\\") { out.append(FileDiff.Line(kind: .meta, text: String(n))); _ = scanner.next() }
                }
                result.append(FileDiff(path: path, isBinary: false, lines: out))
            }
        }
        return result
    }

    /// The diff of one file out of a whole-tree diff, found by a byte search instead of parsing everything.
    /// Returns nil when the file has no section (identical content, or a mode-only change).
    public static func section(of text: String, path: String) -> FileDiff? {
        var t = text
        let needles = ["\n+++ b/\(path)\n", "\n--- a/\(path)\n+++ /dev/null\n", "Binary files a/\(path) and b/\(path) differ\n"]
        let start: Int? = t.withUTF8 { hay -> Int? in
            for (i, n) in needles.enumerated() {
                let needle = Array(n.utf8)
                guard let base = hay.baseAddress, hay.count >= needle.count,
                      let hit = memmem(base, hay.count, needle, needle.count) else { continue }
                var off = base.distance(to: hit.assumingMemoryBound(to: UInt8.self))
                if i == 0 {          // back up over the preceding "--- a/..." line
                    var p = off - 1
                    while p > 0 && base[p - 1] != 0x0A { p -= 1 }
                    off = max(p, 0)
                } else if i < 2 { off += 1 }
                return off
            }
            return nil
        }
        guard let start else { return nil }
        let idx = t.utf8.index(t.utf8.startIndex, offsetBy: start)
        return parse(String(t[idx...]), maxFiles: 1).first
    }

    /// Diff of a file that does not exist in HEAD: the whole file as additions. Binary files and files over
    /// 1 MB get a one-line note instead of their content.
    public static func synthesizeAdded(path: String, root: URL, maxBytes: Int = 1_000_000) -> FileDiff {
        let file = root.appendingPathComponent(path)
        func note(_ s: String) -> FileDiff { FileDiff(path: path, isBinary: true, lines: [FileDiff.Line(kind: .meta, text: s)]) }
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: file.path) else { return note(L("The file is no longer there.")) }
        let size = (attrs[.size] as? NSNumber)?.intValue ?? 0
        if size > maxBytes { return note(LF("New file, %@ (too large to show).", ByteFormat.string(Int64(size)))) }
        guard let data = try? Data(contentsOf: file) else { return note(L("The file cannot be read.")) }
        if data.contains(0) { return note(LF("New binary file, %@.", ByteFormat.string(Int64(size)))) }
        let text = String(decoding: data, as: UTF8.self)
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let body = text.hasSuffix("\n") ? Array(lines.dropLast()) : Array(lines)
        var out: [FileDiff.Line] = [
            FileDiff.Line(kind: .meta, text: "--- /dev/null"), FileDiff.Line(kind: .meta, text: "+++ b/\(path)"),
            FileDiff.Line(kind: .hunk, text: "@@ -0,0 +1,\(body.count) @@"),
        ]
        out.reserveCapacity(body.count + 3)
        for l in body { out.append(FileDiff.Line(kind: .added, text: "+" + l)) }
        return FileDiff(path: path, isBinary: false, lines: out)
    }

    private static func stripPrefix(_ s: String, _ p: String) -> String { s.hasPrefix(p) ? String(s.dropFirst(p.count)) : s }

    /// Parses `@@ -1,3 +1,4 @@` into (old, new) line counts; a missing count means 1.
    static func counts(_ header: String) -> (Int, Int) {
        let parts = header.split(separator: " ")
        func n(_ token: Substring?) -> Int {
            guard let token else { return 0 }
            let body = token.dropFirst()
            let pieces = body.split(separator: ",", omittingEmptySubsequences: false)
            return pieces.count > 1 ? (Int(pieces[1]) ?? 0) : 1
        }
        let old = parts.first { $0.hasPrefix("-") }
        let new = parts.first { $0.hasPrefix("+") }
        return (n(old), n(new))
    }
}

/// Lazy line iterator over a String's UTF-8 view with one line of lookahead.
struct LineScanner {
    private let text: String
    private var index: String.Index
    private var buffered: Substring??

    init(_ text: String) { self.text = text; index = text.startIndex }

    private mutating func read() -> Substring? {
        guard index < text.endIndex else { return nil }
        let utf8 = text.utf8
        if let nl = utf8[index...].firstIndex(of: 0x0A) {
            let line = text[index..<nl]
            index = utf8.index(after: nl)
            return line
        }
        let rest = text[index...]
        index = text.endIndex
        return rest
    }

    mutating func next() -> Substring? {
        if let b = buffered { buffered = nil; return b }
        return read()
    }

    mutating func peek() -> Substring? {
        if let b = buffered { return b }
        let l = read()
        buffered = .some(l)
        return l
    }
}
