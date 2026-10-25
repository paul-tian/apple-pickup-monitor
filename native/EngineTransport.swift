import Foundation
import Darwin

enum EngineTransportError: LocalizedError {
    case notRunning, helperMissing, busy, requestTooLarge, invalidResponse, timeout
    case failed(String)
    case remote(status: Int, message: String)

    var errorDescription: String? {
        switch self {
        case .notRunning: return "The monitoring engine is starting."
        case .helperMissing: return "The monitoring engine is missing from this app. Rebuild or download the complete app."
        case .busy: return "The app is busy. Please try again shortly."
        case .requestTooLarge: return "This request is too large. Select fewer monitoring targets."
        case .invalidResponse: return "The monitoring engine returned an unexpected response."
        case .timeout: return "The monitoring engine took too long to respond."
        case .failed(let message), .remote(_, let message): return message
        }
    }
}

/// A private, app-owned pipe to the bundled engine. No listening socket,
/// discovery file, access token, or independently running service is involved.
@MainActor
final class EngineTransport {
    var onUnexpectedExit: (() -> Void)?
    var isRunning: Bool { process?.isRunning == true && input != nil && !stopping }

    private struct Pending {
        let continuation: CheckedContinuation<Data, Error>
        let timeout: Task<Void, Never>
    }

    private let dataDirectory: URL?
    private let helperURL: URL?
    private let writer = DispatchQueue(label: "com.paultian.apple-pickup-monitor.engine-writer")
    private var process: Process?
    private var input: EnginePipeWriter?
    private var output: FileHandle?
    private var framer: EngineResponseFramer?
    private var pending: [UInt64: Pending] = [:]
    private var nextID: UInt64 = 1
    private var generation: UInt64 = 0
    private var stopping = false
    private var exitReported = false
    private static let maximumRequestBytes = 64 * 1024
    private static let maximumPendingRequests = 32

    init(dataDirectory: URL? = nil, helperURL: URL? = nil) {
        self.dataDirectory = dataDirectory
        self.helperURL = helperURL
    }

    func start() throws {
        if isRunning { return }
        guard process?.isRunning != true else { throw EngineTransportError.busy }
        releaseHandles()
        failAll(EngineTransportError.notRunning)
        generation &+= 1
        let currentGeneration = generation
        stopping = false
        exitReported = false

        let bundledHelper = Bundle.main.executableURL?
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Helpers/apple-pickup-monitor")
        guard let executable = helperURL ?? bundledHelper,
              FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw EngineTransportError.helperMissing
        }

        let log = try openLog()
        defer { try? log.close() }
        let stdinPipe = Pipe()
        let stdoutPipe = Pipe()
        // Explicitly prevent the engine from inheriting the parent's stdin
        // writer; otherwise it could keep itself alive after the app crashes.
        for handle in [stdinPipe.fileHandleForWriting, stdoutPipe.fileHandleForReading] {
            let flags = fcntl(handle.fileDescriptor, F_GETFD)
            if flags >= 0 { _ = fcntl(handle.fileDescriptor, F_SETFD, flags | FD_CLOEXEC) }
        }
        let child = Process()
        child.executableURL = executable
        child.arguments = ["--native"] + (dataDirectory.map { ["--data-dir", $0.path] } ?? [])
        child.standardInput = stdinPipe
        child.standardOutput = stdoutPipe
        child.standardError = log
        var environment = ProcessInfo.processInfo.environment
        environment.removeValue(forKey: "__CFBundleIdentifier")
        child.environment = environment

        // FileHandle writes to a closed pipe must throw EPIPE, never terminate
        // the native app with SIGPIPE. This applies only to our own pipe fd.
        _ = fcntl(stdinPipe.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1)
        input = EnginePipeWriter(handle: stdinPipe.fileHandleForWriting)
        output = stdoutPipe.fileHandleForReading
        process = child
        let reader = EngineResponseFramer(handle: stdoutPipe.fileHandleForReading) { [weak self] frames, ended, malformed in
            DispatchQueue.main.async { [weak self] in
                guard let self, self.generation == currentGeneration else { return }
                for frame in frames { self.receive(frame, generation: currentGeneration) }
                if malformed { self.transportFailed(.invalidResponse, generation: currentGeneration) }
                else if ended { self.transportFailed(.failed("The monitoring engine stopped unexpectedly."), generation: currentGeneration) }
            }
        }
        framer = reader
        child.terminationHandler = { [weak self] _ in
            // Normally stdout EOF follows all responses. This delayed fallback
            // also handles a child that exits while another process holds stdout.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { [weak self] in
                guard let self, self.generation == currentGeneration, !self.exitReported else { return }
                self.transportFailed(.failed("The monitoring engine stopped unexpectedly."), generation: currentGeneration)
            }
        }
        do {
            try child.run()
            // Only the child retains stdin's read end and stdout's write end.
            // Closing our unused copies is essential for EOF-based lifetime control.
            try? stdinPipe.fileHandleForReading.close()
            try? stdoutPipe.fileHandleForWriting.close()
            reader.start()
        } catch {
            child.terminationHandler = nil
            releaseHandles()
            process = nil
            throw EngineTransportError.failed("The monitoring engine could not start. Check the app’s logs folder.")
        }
    }

    func request(path: String, method: String = "GET", body: [String: Any]? = nil,
                 timeout: TimeInterval = 8) async throws -> Data {
        try Task.checkCancellation()
        guard isRunning, let pipe = input else { throw EngineTransportError.notRunning }
        guard pending.count < Self.maximumPendingRequests else { throw EngineTransportError.busy }
        guard path.hasPrefix("/api/"), !path.contains("\n"), !path.contains("\r"),
              ["GET", "POST", "PUT"].contains(method) else { throw EngineTransportError.invalidResponse }
        let id = nextID
        nextID = nextID == UInt64.max ? 1 : nextID + 1
        let currentGeneration = generation
        var message: [String: Any] = ["id": id, "method": method, "path": path]
        if let body { message["body"] = body }
        guard JSONSerialization.isValidJSONObject(message) else { throw EngineTransportError.invalidResponse }
        var frame = try JSONSerialization.data(withJSONObject: message)
        frame.append(0x0a)
        guard frame.count <= Self.maximumRequestBytes else { throw EngineTransportError.requestTooLarge }
        guard timeout.isFinite else { throw EngineTransportError.invalidResponse }
        let boundedTimeout = min(max(timeout, 0.05), 120)
        let wireFrame = frame

        return try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in
                let timer = Task { [weak self] in
                    do { try await Task.sleep(nanoseconds: UInt64(boundedTimeout * 1_000_000_000)) }
                    catch { return }
                    guard let self, self.generation == currentGeneration else { return }
                    self.complete(id: id, result: .failure(EngineTransportError.timeout))
                }
                pending[id] = Pending(continuation: continuation, timeout: timer)
                if Task.isCancelled {
                    complete(id: id, result: .failure(CancellationError()))
                    return
                }
                writer.async { [weak self] in
                    do { try pipe.write(wireFrame) }
                    catch {
                        DispatchQueue.main.async { [weak self] in
                            self?.transportFailed(.failed("The monitoring engine stopped unexpectedly."), generation: currentGeneration)
                        }
                    }
                }
            }
        }, onCancel: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, self.generation == currentGeneration else { return }
                self.complete(id: id, result: .failure(CancellationError()))
            }
        })
    }

    func stop() async {
        guard !stopping else { return }
        // EOF is the definitive shutdown command, including while a request
        // is waiting for Keychain access. Do not await another queued request.
        stopping = true
        exitReported = true
        failAll(CancellationError())
        releaseHandles()
        let child = process
        for _ in 0..<10 {
            guard child?.isRunning == true else { break }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        if child?.isRunning == true { child?.terminate() }
        for _ in 0..<5 {
            guard child?.isRunning == true else { break }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        if let child, child.isRunning {
            // Last resort is limited to the exact child Process we created.
            _ = Darwin.kill(child.processIdentifier, SIGKILL)
        }
        process?.terminationHandler = nil
        process = nil
    }

    private func receive(_ frame: Data, generation expectedGeneration: UInt64) {
        guard generation == expectedGeneration, !exitReported else { return }
        do {
            let response = try JSONDecoder().decode(EngineResponse.self, from: frame)
            guard pending[response.id] != nil else { return } // timed out/cancelled response
            guard (100...599).contains(response.status) else { throw EngineTransportError.invalidResponse }
            let body = try JSONEncoder().encode(response.body)
            if (200..<300).contains(response.status) { complete(id: response.id, result: .success(body)) }
            else {
                let error = (try? JSONDecoder().decode(EngineErrorBody.self, from: body).error)
                    ?? "The monitoring engine could not complete this request."
                complete(id: response.id, result: .failure(EngineTransportError.remote(status: response.status, message: error)))
            }
        } catch { transportFailed(.invalidResponse, generation: expectedGeneration) }
    }

    private func complete(id: UInt64, result: Result<Data, Error>) {
        guard let entry = pending.removeValue(forKey: id) else { return }
        entry.timeout.cancel()
        entry.continuation.resume(with: result)
    }

    private func failAll(_ error: Error) {
        let entries = pending.values
        pending.removeAll()
        for entry in entries {
            entry.timeout.cancel()
            entry.continuation.resume(throwing: error)
        }
    }

    private func transportFailed(_ error: EngineTransportError, generation expectedGeneration: UInt64) {
        guard generation == expectedGeneration, !exitReported else { return }
        exitReported = true
        failAll(error)
        releaseHandles()
        if process?.isRunning == true { process?.terminate() }
        if !stopping { onUnexpectedExit?() }
    }

    private func releaseHandles() {
        framer?.stop()
        framer = nil
        input?.close()
        try? output?.close()
        input = nil
        output = nil
    }

    private func openLog() throws -> FileHandle {
        let logs = dataDirectory?.appendingPathComponent("logs", isDirectory: true)
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/Apple Pickup Monitor", isDirectory: true)
        try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: logs.path)
        let url = logs.appendingPathComponent("native-helper.log")
        if let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size > 1_048_576 {
            let previous = logs.appendingPathComponent("native-helper.previous.log")
            try? FileManager.default.removeItem(at: previous)
            try? FileManager.default.moveItem(at: url, to: previous)
        }
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        let file = try FileHandle(forWritingTo: url)
        try file.seekToEnd()
        return file
    }
}

/// Nonblocking writes avoid trapping the main actor in FileHandle.close() when
/// an unhealthy child stops consuming input. A partial frame failure closes the
/// whole transport; it is never retried against a replacement engine.
private final class EnginePipeWriter: @unchecked Sendable {
    private let handle: FileHandle
    private let lock = NSLock()
    private var closed = false
    init(handle: FileHandle) {
        self.handle = handle
        let flags = fcntl(handle.fileDescriptor, F_GETFL)
        if flags >= 0 { _ = fcntl(handle.fileDescriptor, F_SETFL, flags | O_NONBLOCK) }
    }
    func close() {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return }
        closed = true
        try? handle.close()
    }
    func write(_ data: Data) throws {
        let deadline = Date().addingTimeInterval(2)
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return }
            var offset = 0
            while offset < bytes.count {
                lock.lock()
                guard !closed else { lock.unlock(); throw EngineTransportError.notRunning }
                let fd = handle.fileDescriptor
                let count = Darwin.write(fd, base.advanced(by: offset), bytes.count - offset)
                let code = errno
                lock.unlock()
                if count > 0 { offset += count; continue }
                if count < 0 && code == EINTR { continue }
                if count < 0 && (code == EAGAIN || code == EWOULDBLOCK), Date() < deadline {
                    var descriptor = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
                    _ = Darwin.poll(&descriptor, 1, 100)
                    continue
                }
                throw EngineTransportError.failed("The monitoring engine stopped accepting requests.")
            }
        }
    }
}

/// Framing stays off the main actor. Serialize reads and callback submission so
/// a final response is always delivered before the EOF that follows it.
private final class EngineResponseFramer: @unchecked Sendable {
    private let handle: FileHandle
    private let deliver: ([Data], Bool, Bool) -> Void
    private let lock = NSLock()
    private var buffer = Data()
    private var ended = false
    private static let maximumFrameBytes = 8 * 1024 * 1024

    init(handle: FileHandle, deliver: @escaping ([Data], Bool, Bool) -> Void) {
        self.handle = handle
        self.deliver = deliver
    }
    func start() {
        handle.readabilityHandler = { [weak self] handle in self?.read(handle) }
    }
    func stop() {
        handle.readabilityHandler = nil
        lock.lock(); ended = true; buffer.removeAll(); lock.unlock()
    }
    private func read(_ handle: FileHandle) {
        lock.lock()
        defer { lock.unlock() }
        guard !ended else { return }
        let chunk = handle.availableData
        if chunk.isEmpty {
            ended = true
            handle.readabilityHandler = nil
            deliver([], true, !buffer.isEmpty)
            buffer.removeAll()
            return
        }
        // Only newly appended bytes can contain an unseen delimiter. Scanning
        // the entire accumulated buffer on each callback is quadratic for a
        // large catalogue or malformed line without a newline.
        var searchOffset = buffer.count
        buffer.append(chunk)
        var frames: [Data] = []
        while let newline = buffer[buffer.index(buffer.startIndex, offsetBy: searchOffset)...].firstIndex(of: 0x0a) {
            let count = buffer.distance(from: buffer.startIndex, to: newline)
            guard count <= Self.maximumFrameBytes else {
                ended = true; handle.readabilityHandler = nil; buffer.removeAll()
                deliver(frames, false, true); return
            }
            let frame = Data(buffer[..<newline])
            buffer.removeSubrange(...newline)
            searchOffset = 0
            if !frame.isEmpty { frames.append(frame) }
        }
        guard buffer.count <= Self.maximumFrameBytes else {
            ended = true; handle.readabilityHandler = nil; buffer.removeAll()
            deliver(frames, false, true); return
        }
        if !frames.isEmpty { deliver(frames, false, false) }
    }
}

private struct EngineResponse: Decodable {
    let id: UInt64
    let status: Int
    let body: EngineJSON
}
private struct EngineErrorBody: Decodable { let error: String }
private indirect enum EngineJSON: Codable {
    case null, bool(Bool), integer(Int64), unsigned(UInt64), number(Double), string(String)
    case array([EngineJSON]), object([String: EngineJSON])
    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let item = try? value.decode(Bool.self) { self = .bool(item) }
        else if let item = try? value.decode(Int64.self) { self = .integer(item) }
        else if let item = try? value.decode(UInt64.self) { self = .unsigned(item) }
        else if let item = try? value.decode(Double.self) { self = .number(item) }
        else if let item = try? value.decode(String.self) { self = .string(item) }
        else if let item = try? value.decode([EngineJSON].self) { self = .array(item) }
        else { self = .object(try value.decode([String: EngineJSON].self)) }
    }
    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .null: try value.encodeNil()
        case .bool(let item): try value.encode(item)
        case .integer(let item): try value.encode(item)
        case .unsigned(let item): try value.encode(item)
        case .number(let item): try value.encode(item)
        case .string(let item): try value.encode(item)
        case .array(let item): try value.encode(item)
        case .object(let item): try value.encode(item)
        }
    }
}
