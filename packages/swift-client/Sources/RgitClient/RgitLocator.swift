import Foundation

/// Finds the `rgit` executable: explicit override, then the app bundle, then common install paths.
public enum RgitLocator {
    public static func find(bundle: Bundle = .main, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL? {
        let fm = FileManager.default
        var candidates: [URL] = []
        if let override = environment["RGIT_BINARY"], !override.isEmpty {
            candidates.append(URL(fileURLWithPath: override))
        }
        if let res = bundle.resourceURL { candidates.append(res.appendingPathComponent("rgit")) }
        if let exe = bundle.executableURL { candidates.append(exe.deletingLastPathComponent().appendingPathComponent("rgit")) }
        if let helpers = bundle.url(forAuxiliaryExecutable: "rgit") { candidates.append(helpers) }
        for dir in (environment["PATH"] ?? "").split(separator: ":") {
            candidates.append(URL(fileURLWithPath: String(dir)).appendingPathComponent("rgit"))
        }
        for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
            candidates.append(URL(fileURLWithPath: dir).appendingPathComponent("rgit"))
        }
        return candidates.first { fm.isExecutableFile(atPath: $0.path) }
    }
}
