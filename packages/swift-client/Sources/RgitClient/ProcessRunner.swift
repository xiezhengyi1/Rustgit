import Foundation

/// Callback for NDJSON progress events. Called on an arbitrary thread; hop to the main actor yourself.
public typealias ProgressHandler = @Sendable (ProgressEvent) -> Void

/// Thread-safe handle on a running child process, used by the cancellation handler.
final class ProcessBox: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var stdin: FileHandle?
    private var cancelled = false

    func attach(_ p: Process, stdin: FileHandle?) {
        lock.lock(); defer { lock.unlock() }
        process = p
        self.stdin = stdin
        if cancelled { Self.kill(p, stdin: stdin) }
    }

    func cancel() {
        lock.lock(); defer { lock.unlock() }
        cancelled = true
        if let p = process { Self.kill(p, stdin: stdin) }
    }

    var wasCancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }

    /// Closing stdin and sending SIGTERM is what docs/gui-interface.md defines as cancellation;
    /// SIGKILL follows if the engine is still around after a grace period.
    static func kill(_ p: Process, stdin: FileHandle?) {
        try? stdin?.close()
        guard p.isRunning else { return }
        p.terminate()
        let pid = p.processIdentifier
        DispatchQueue.global().asyncAfter(deadline: .now() + 3) {
            if p.isRunning { Foundation.kill(pid, SIGKILL) }
        }
    }
}

/// Collects up to `limit` bytes of a pipe (stderr) without ever blocking the child.
final class BoundedCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var data = Data()
    private let limit: Int
    init(limit: Int = 64 * 1024) { self.limit = limit }
    func append(_ d: Data) {
        lock.lock(); defer { lock.unlock() }
        if data.count < limit { data.append(d.prefix(limit - data.count)) }
    }
    var string: String {
        lock.lock(); defer { lock.unlock() }
        return String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct RawOutcome {
    var resultLine: Data?
    var exitCode: Int32
    var stderr: String
    var cancelled: Bool
}

/// Parsed form of one NDJSON line.
enum WireLine {
    case event(ProgressEvent)
    case result(Data)
    case other

    static func parse(_ line: Data) -> WireLine {
        guard let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { return .other }
        if obj["ok"] is Bool { return .result(line) }
        if let name = obj["event"] as? String {
            var raw: [String: String] = [:]
            for (k, v) in obj { raw[k] = "\(v)" }
            return .event(ProgressEvent(
                event: name,
                phase: obj["phase"] as? String,
                done: (obj["done"] as? NSNumber)?.int64Value,
                total: (obj["total"] as? NSNumber)?.int64Value,
                raw: raw,
                connection: name == "connection" ? try? RgitClient.decoder.decode(ConnectionInfo.self, from: line) : nil,
                host: (obj["host"] as? [String: Any]).flatMap { try? JSONSerialization.data(withJSONObject: $0) }.flatMap { try? RgitClient.decoder.decode(DiscoveredHost.self, from: $0) }))
        }
        return .other
    }
}

/// Splits a pipe into lines on a dedicated thread. `FileHandle.bytes` is deliberately not used: its
/// blocking reads share one serial queue, so a long-lived `rgit serve` reader would starve every
/// other reader in the process.
func lines(of handle: FileHandle) -> AsyncStream<String> {
    AsyncStream { cont in
        let thread = Thread {
            let fd = handle.fileDescriptor
            var buf = Data()
            var chunk = [UInt8](repeating: 0, count: 64 * 1024)
            while true {
                let n = chunk.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
                if n < 0 && errno == EINTR { continue }
                if n <= 0 { break }
                buf.append(contentsOf: chunk[0..<n])
                while let nl = buf.firstIndex(of: 0x0A) {
                    cont.yield(String(decoding: buf[buf.startIndex..<nl], as: UTF8.self))
                    buf = Data(buf[buf.index(after: nl)...])
                }
            }
            if !buf.isEmpty { cont.yield(String(decoding: buf, as: UTF8.self)) }
            cont.finish()
            withExtendedLifetime(handle) {}
        }
        thread.name = "rgit-stdout"
        thread.start()
    }
}

/// Awaitable process exit. `Process.waitUntilExit()` can miss the termination wake-up when it is
/// called from a thread other than the launcher's (observed: a finished child never "exits"), so
/// exit is observed through `terminationHandler` instead.
final class ExitSignal: @unchecked Sendable {
    private let lock = NSLock()
    private var status: Int32?
    private var waiters: [CheckedContinuation<Int32, Never>] = []

    func fire(_ code: Int32) {
        lock.lock()
        status = code
        let w = waiters
        waiters = []
        lock.unlock()
        for c in w { c.resume(returning: code) }
    }

    @discardableResult
    func wait() async -> Int32 {
        await withCheckedContinuation { (c: CheckedContinuation<Int32, Never>) in
            lock.lock()
            if let status {
                lock.unlock()
                c.resume(returning: status)
            } else {
                waiters.append(c)
                lock.unlock()
            }
        }
    }
}

/// A started child with the parent's ends of its three pipes.
struct Spawned {
    var process: Process
    var stdout: FileHandle
    var stderr: FileHandle
    var stdin: FileHandle
    var exit: ExitSignal
}

/// All spawns go through this lock together with pipe creation: a pipe is made close-on-exec
/// before another thread can fork, so a long-running child (`rgit serve`) never inherits the
/// pipes of a concurrent command, which would keep that command's stdout from ever reaching EOF.
private let spawnLock = NSLock()

private func makePipe() -> (read: FileHandle, write: FileHandle) {
    var fds: [Int32] = [0, 0]
    precondition(pipe(&fds) == 0, "pipe() failed")
    _ = fcntl(fds[0], F_SETFD, FD_CLOEXEC)
    _ = fcntl(fds[1], F_SETFD, FD_CLOEXEC)
    return (FileHandle(fileDescriptor: fds[0], closeOnDealloc: true),
            FileHandle(fileDescriptor: fds[1], closeOnDealloc: true))
}

func spawn(executable: URL, arguments: [String], environment: [String: String]?) throws -> Spawned {
    let p = Process()
    p.executableURL = executable
    p.arguments = arguments
    var env = ProcessInfo.processInfo.environment
    env["NO_COLOR"] = "1"
    for (k, v) in environment ?? [:] { env[k] = v }
    p.environment = env

    let exit = ExitSignal()
    p.terminationHandler = { exit.fire($0.terminationStatus) }

    spawnLock.lock()
    defer { spawnLock.unlock() }
    let out = makePipe(), err = makePipe(), inp = makePipe()
    p.standardOutput = out.write
    p.standardError = err.write
    p.standardInput = inp.read
    do { try p.run() } catch {
        for h in [out.read, out.write, err.read, err.write, inp.read, inp.write] { try? h.close() }
        throw RgitError.executableUnavailable("\(executable.path): \(error.localizedDescription)")
    }
    // The child has its own copies; close ours so EOF is seen when it exits.
    try? out.write.close()
    try? err.write.close()
    try? inp.read.close()
    return Spawned(process: p, stdout: out.read, stderr: err.read, stdin: inp.write, exit: exit)
}

/// Runs `rgit` to completion. Streams events to `onEvent`; returns the final `{"ok":...}` line.
func runToCompletion(executable: URL, arguments: [String], environment: [String: String]?,
                     onEvent: ProgressHandler?, input: Data? = nil) async throws -> RawOutcome {
    let box = ProcessBox()
    return try await withTaskCancellationHandler {
        let child = try spawn(executable: executable, arguments: arguments, environment: environment)
        let process = child.process
        box.attach(process, stdin: child.stdin)
        let stderr = BoundedCollector()
        child.stderr.readabilityHandler = { h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil } else { stderr.append(d) }
        }

        if let input {
            try child.stdin.write(contentsOf: input)
            try child.stdin.close()
        }
        var result: Data?
        for await line in lines(of: child.stdout) {
            guard !line.isEmpty else { continue }
            switch WireLine.parse(Data(line.utf8)) {
            case .event(let e): onEvent?(e)
            case .result(let d): result = d
            case .other: break
            }
        }
        let exitCode = await child.exit.wait()
        child.stderr.readabilityHandler = nil
        if let rest = try? child.stderr.readToEnd(), !rest.isEmpty { stderr.append(rest) }
        try? child.stdin.close()
        return RawOutcome(resultLine: result, exitCode: exitCode,
                          stderr: stderr.string, cancelled: box.wasCancelled || Task.isCancelled)
    } onCancel: {
        box.cancel()
    }
}
