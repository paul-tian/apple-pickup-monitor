import Foundation
import Darwin

private struct TransportTestFailure: Error { let message: String }
private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw TransportTestFailure(message: message) }
}

@main
struct TransportTests {
    @MainActor
    static func main() async {
        do {
            if CommandLine.arguments[1] == "--abrupt-parent" {
                let helper = URL(fileURLWithPath: CommandLine.arguments[2])
                let directory = URL(fileURLWithPath: CommandLine.arguments[3], isDirectory: true)
                let transport = EngineTransport(dataDirectory: directory, helperURL: helper)
                try transport.start()
                _ = try await transport.request(path: "/api/echo", method: "POST", body: ["ready": true])
                Darwin._exit(0) // No Swift/Process cleanup: only kernel fd closure.
            }
            let helper = URL(fileURLWithPath: CommandLine.arguments[1])
            let root = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
            let engine = EngineTransport(dataDirectory: root, helperURL: helper)
            var unexpectedExits = 0
            engine.onUnexpectedExit = { unexpectedExits += 1 }
            try engine.start()
            try expect(engine.isRunning, "Engine did not start")

            let echoed = try await engine.request(path: "/api/echo", method: "POST", body: ["text": "门店 🛍️", "count": 3])
            let object = try JSONSerialization.jsonObject(with: echoed) as! [String: Any]
            try expect(object["text"] as? String == "门店 🛍️", "Chunked UTF-8 response was corrupted")
            try expect(object["count"] as? Int == 3, "JSON body numeric type changed")

            do {
                _ = try await engine.request(path: "/api/error")
                throw TransportTestFailure(message: "Remote error was accepted as success")
            } catch EngineTransportError.remote(let status, let message) {
                try expect(status == 422 && message == "A safe provider error.", "Remote error was not preserved")
            }

            do {
                _ = try await engine.request(path: "/api/delayed", timeout: 0.05)
                throw TransportTestFailure(message: "Slow request did not time out")
            } catch EngineTransportError.timeout { }
            let next = try await engine.request(path: "/api/echo", method: "POST", body: ["fresh": true])
            let nextObject = try JSONSerialization.jsonObject(with: next) as! [String: Any]
            try expect(nextObject["fresh"] as? Bool == true, "Late reply was matched to the next request")
            try await Task.sleep(nanoseconds: 250_000_000)
            try expect(engine.isRunning, "Late timed-out reply stopped the engine")

            let canceled = Task { try await engine.request(path: "/api/hang", timeout: 10) }
            try await Task.sleep(nanoseconds: 20_000_000)
            canceled.cancel()
            do { _ = try await canceled.value; throw TransportTestFailure(message: "Cancellation did not finish the request") }
            catch is CancellationError { }
            try expect(engine.isRunning, "Canceling a request stopped the engine")

            do {
                _ = try await engine.request(path: "/api/echo", method: "POST", body: ["large": String(repeating: "x", count: 70_000)])
                throw TransportTestFailure(message: "Oversized request was accepted")
            } catch EngineTransportError.requestTooLarge { }

            let pending = Task { try await engine.request(path: "/api/hang", timeout: 30) }
            try await Task.sleep(nanoseconds: 20_000_000)
            let stoppedAt = Date()
            await engine.stop()
            try expect(Date().timeIntervalSince(stoppedAt) < 3, "Shutdown did not remain bounded")
            do { _ = try await pending.value; throw TransportTestFailure(message: "Shutdown left a request pending") }
            catch is CancellationError { }
            try expect(unexpectedExits == 0, "Intentional stop triggered recovery")
            try expect(FileManager.default.fileExists(atPath: root.appendingPathComponent("eof-observed").path), "Child did not observe stdin EOF")

            try engine.start()
            _ = try await engine.request(path: "/api/stop-reading")
            let blocked = (0..<12).map { _ in
                Task { try await engine.request(path: "/api/echo", method: "POST", body: ["large": String(repeating: "x", count: 60_000)], timeout: 30) }
            }
            try await Task.sleep(nanoseconds: 50_000_000)
            let blockedStop = Date()
            await engine.stop()
            try expect(Date().timeIntervalSince(blockedStop) < 3, "Blocked pipe writes hung shutdown")
            for task in blocked { _ = try? await task.value }
            try expect(unexpectedExits == 0, "Stopping a stalled engine triggered recovery")

            for route in ["/api/malformed", "/api/oversized", "/api/crash"] {
                try engine.start()
                do {
                    _ = try await engine.request(path: route, timeout: 4)
                    throw TransportTestFailure(message: "Invalid/crashed engine response was accepted")
                } catch is EngineTransportError { }
                try await Task.sleep(nanoseconds: 200_000_000)
                await engine.stop()
            }
            try expect(unexpectedExits == 3, "Each failed process should report exactly one unexpected exit; got \(unexpectedExits)")

            let crashDirectory = root.appendingPathComponent("parent-exit", isDirectory: true)
            try FileManager.default.createDirectory(at: crashDirectory, withIntermediateDirectories: true)
            let parent = Process()
            parent.executableURL = Bundle.main.executableURL
            parent.arguments = ["--abrupt-parent", helper.path, crashDirectory.path]
            parent.standardInput = FileHandle.nullDevice
            parent.standardOutput = FileHandle.nullDevice
            parent.standardError = FileHandle.nullDevice
            try parent.run()
            let deadline = Date().addingTimeInterval(5)
            let marker = crashDirectory.appendingPathComponent("eof-observed").path
            while Date() < deadline && !FileManager.default.fileExists(atPath: marker) {
                try await Task.sleep(nanoseconds: 50_000_000)
            }
            try expect(FileManager.default.fileExists(atPath: marker), "Engine retained stdin after abrupt app exit")
            print("Native pipe transport tests passed: UTF-8 framing, IDs, errors, timeout, cancellation, limits, EOF, blocked writes, parent exit and recovery.")
        } catch let failure as TransportTestFailure {
            fputs("Native pipe transport test failed: \(failure.message)\n", stderr)
            exit(1)
        } catch {
            fputs("Native pipe transport test failed: \(error)\n", stderr)
            exit(1)
        }
    }
}
