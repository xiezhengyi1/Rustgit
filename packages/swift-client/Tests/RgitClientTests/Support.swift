import Foundation
@testable import RgitClient

/// Locates the real `rgit` under test: `RGIT_BINARY`, else the workspace's target/release or target/debug.
enum TestEnv {
    static let rgit: URL = {
        if let p = ProcessInfo.processInfo.environment["RGIT_BINARY"], !p.isEmpty {
            return URL(fileURLWithPath: p)
        }
        var dir = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { dir.deleteLastPathComponent() }   // .../packages/swift-client/Tests/RgitClientTests/Support.swift -> repo root's parent chain
        // #filePath = <root>/packages/swift-client/Tests/RgitClientTests/Support.swift
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        for sub in ["target/release/rgit", "target/debug/rgit"] {
            let u = root.appendingPathComponent(sub)
            if FileManager.default.isExecutableFile(atPath: u.path) { return u }
        }
        return root.appendingPathComponent("target/release/rgit")
    }()
}

/// A temporary directory removed on deinit.
final class Sandbox {
    let url: URL
    init() throws {
        url = FileManager.default.temporaryDirectory.appendingPathComponent("rgit-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }
    deinit { try? FileManager.default.removeItem(at: url) }

    func dir(_ name: String) throws -> URL {
        let d = url.appendingPathComponent(name)
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    static func write(_ text: String, to path: String, in dir: URL) throws {
        let f = dir.appendingPathComponent(path)
        try FileManager.default.createDirectory(at: f.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: f, atomically: true, encoding: .utf8)
    }

    static func read(_ path: String, in dir: URL) throws -> String {
        try String(contentsOf: dir.appendingPathComponent(path), encoding: .utf8)
    }
}

func makeClient(in dir: URL) -> RgitClient { RgitClient(executable: TestEnv.rgit, directory: dir, environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": dir.appendingPathComponent(".rgit/test-keys").path]) }

/// An initialised project with user.name/email set and one commit.
func makeProject(_ box: Sandbox, name: String = "proj", files: [String: String] = ["a.txt": "one\ntwo\nthree\n"]) async throws -> (RgitClient, URL) {
    let dir = try box.dir(name)
    let c = makeClient(in: dir)
    _ = try await c.initProject()
    try await c.setConfig("user.name", "Test User")
    try await c.setConfig("user.email", "test@example.com")
    for (p, t) in files { try Sandbox.write(t, to: p, in: dir) }
    if !files.isEmpty { _ = try await c.commit(message: "initial") }
    return (c, dir)
}
