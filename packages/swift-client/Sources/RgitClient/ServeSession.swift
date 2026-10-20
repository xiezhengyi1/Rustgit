import Foundation

/// A running `rgit serve`: the project-hosting process. It lives as long as this object (or until
/// `stop()`); it does not depend on any window. Hosting is unavailable while the Mac sleeps or the
/// owning app/process is not running.
public final class ServeSession: @unchecked Sendable {
    public let process: Process
    /// Address the host is listening on, e.g. `127.0.0.1:7878`.
    public let address: String
    public let projectID: String
    public let fingerprint: String?
    /// Events from the host (connections, pushes, errors); finishes with `.exited` when the process ends.
    public let events: AsyncStream<ServeEvent>

    private let box = ProcessBox()
    private let exit: ExitSignal
    private let continuation: AsyncStream<ServeEvent>.Continuation

    init(process: Process, stdin: FileHandle, exit: ExitSignal, address: String, projectID: String, fingerprint: String?,
         events: AsyncStream<ServeEvent>, continuation: AsyncStream<ServeEvent>.Continuation) {
        self.process = process
        self.exit = exit
        self.address = address
        self.projectID = projectID
        self.fingerprint = fingerprint
        self.events = events
        self.continuation = continuation
        box.attach(process, stdin: stdin)
    }

    public var isRunning: Bool { process.isRunning }
    public var processIdentifier: Int32 { process.processIdentifier }

    public struct Options: Sendable {
        public var listen: String
        public var initialize: Bool
        public var allowForce: Bool
        public var allowDelete: Bool
        public var insecureListenAny: Bool
        public var insecureOpenAccess: Bool
        public var advertise: Bool
        public var relay: String?
        public init(listen: String = "127.0.0.1:7878", initialize: Bool = false, allowForce: Bool = false,
                    allowDelete: Bool = false, insecureListenAny: Bool = false, insecureOpenAccess: Bool = false, advertise: Bool = false, relay: String? = nil) {
            self.listen = listen; self.initialize = initialize; self.allowForce = allowForce
            self.allowDelete = allowDelete; self.insecureListenAny = insecureListenAny
            self.insecureOpenAccess = insecureOpenAccess; self.advertise = advertise; self.relay = relay
        }
    }

    /// Starts `rgit --json serve` for the bare repository `directory` and waits until it reports
    /// `listening`. If it exits first (port in use, repository locked, bad address) the engine's
    /// structured error is thrown.
    public static func start(executable: URL, directory: URL, options: Options = Options(),
                             environment: [String: String]? = nil) async throws -> ServeSession {
        var args = ["--json", "serve", directory.path, "--listen", options.listen]
        if options.initialize { args.append("--init") }
        if options.allowForce { args.append("--allow-force") }
        if options.allowDelete { args.append("--allow-delete") }
        if options.insecureOpenAccess { args.append("--insecure-open-access") }
        if options.advertise { args.append("--advertise") }
        if let relay = options.relay, !relay.isEmpty { args += ["--iroh", "--iroh-relay", relay] }

        let child = try spawn(executable: executable, arguments: args, environment: environment)
        let process = child.process
        let stderr = BoundedCollector()
        child.stderr.readabilityHandler = { h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil } else { stderr.append(d) }
        }

        let (stream, continuation) = AsyncStream<ServeEvent>.makeStream(bufferingPolicy: .bufferingNewest(500))
        let startup = AsyncStream<Result<(String, String, String?), RgitError>>.makeStream()

        Task.detached {
            var announced = false
            var failure: RgitError?
            do {
                for await line in lines(of: child.stdout) {
                    guard let data = line.isEmpty ? nil : line.data(using: .utf8),
                          let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
                    if obj["ok"] as? Bool == false, let e = obj["error"] as? [String: Any] {
                        failure = .engine(code: RgitErrorCode(wire: e["code"] as? String ?? "other"),
                                          message: e["message"] as? String ?? "",
                                          paths: e["paths"] as? [String] ?? [])
                        continue
                    }
                    guard let name = obj["event"] as? String else { continue }
                    let text = (obj["message"] as? String) ?? line
                    switch name {
                    case "listening":
                        let addr = obj["addr"] as? String ?? ""
                        let proj = obj["project"] as? String ?? ""
                        if !announced {
                            announced = true
                            startup.continuation.yield(.success((addr, proj, obj["fingerprint"] as? String)))
                        }
                        continuation.yield(.listening(addr: addr, project: proj))
                    case "connection": continuation.yield(.connection(text))
                    case "push": continuation.yield(.push(text))
                    case "error": continuation.yield(.error(text))
                    default: continuation.yield(.other(event: name, line: line))
                    }
                }
            }
            let exitCode = await child.exit.wait()
            child.stderr.readabilityHandler = nil
            if !announced {
                startup.continuation.yield(.failure(failure ?? .processFailed(exitCode: exitCode, stderr: stderr.string)))
            }
            continuation.yield(.exited(status: exitCode))
            continuation.finish()
            startup.continuation.finish()
        }

        let box = ProcessBox()
        box.attach(process, stdin: child.stdin)
        return try await withTaskCancellationHandler {
            var first: Result<(String, String, String?), RgitError>?
            for await r in startup.stream { first = r; break }
            switch first {
            case .success(let (addr, proj, fingerprint))?:
                return ServeSession(process: process, stdin: child.stdin, exit: child.exit, address: addr, projectID: proj, fingerprint: fingerprint, events: stream, continuation: continuation)
            case .failure(let e)?:
                throw e
            case nil:
                throw RgitError.processFailed(exitCode: -1, stderr: stderr.string)
            }
        } onCancel: {
            box.cancel()
        }
    }

    /// Stops hosting: closes stdin, sends SIGTERM, escalates to SIGKILL after 3 s. Returns after exit.
    public func stop() async {
        guard process.isRunning else { return }
        box.cancel()
        await exit.wait()
    }
}
