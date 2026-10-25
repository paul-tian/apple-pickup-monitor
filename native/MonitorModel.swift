import AppKit
import Foundation
import Combine

@MainActor
final class MonitorModel: ObservableObject {
    static let shared = MonitorModel()
    @Published var state = MonitorState.empty
    @Published var catalog = CatalogPayload.empty
    @Published var regions: [RegionItem] = []
    @Published var selectedRegion = UserDefaults.standard.string(forKey: "selectedRegion") ?? "en_AU"
    @Published var isCatalogLoading = false
    @Published var isReady = false
    @Published var isRecovering = true
    @Published var isBusy = false
    @Published var errorMessage: String?
    @Published var successMessage: String?
    @Published var engineStatus = "Starting monitoring engine…"
    @Published private(set) var resumingMonitoring = false

    private let engine: EngineTransport
    private var lifecycleTask: Task<Void, Never>?
    private var refreshing = false
    private var shuttingDown = false
    private var desiredRunning = false
    private var pollFailures = 0
    private var backendResumeAt: Date?
    private var catalogGeneration = 0
    private var languageSyncing = false
    let dataDirectory: URL

    init(dataDirectory: URL? = nil, helperURL: URL? = nil) {
        let args = ProcessInfo.processInfo.arguments
        let argumentDirectory: URL? = args.lastIndex(of: "--data-dir").flatMap { index in
            guard args.indices.contains(index + 1) else { return nil }
            return URL(fileURLWithPath: args[index + 1], isDirectory: true)
        }
        let custom = dataDirectory ?? argumentDirectory
        self.dataDirectory = custom ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Apple Pickup Monitor", isDirectory: true)
        engine = EngineTransport(dataDirectory: custom, helperURL: helperURL)
        engine.onUnexpectedExit = { [weak self] in self?.engineFailed() }
    }

    /// This task belongs to the application, not to the lifetime of its window.
    func start() async {
        guard lifecycleTask == nil, !shuttingDown else { return }
        lifecycleTask = Task { [weak self] in await self?.runLifecycle() }
    }

    private func runLifecycle() async {
        var failedStarts = 0
        var hasLaunched = false
        while !Task.isCancelled && !shuttingDown {
            if !isReady {
                engineStatus = !hasLaunched && failedStarts == 0 ? "Starting monitoring engine…" : "Restarting monitoring engine…"
                isRecovering = true
                do {
                    await engine.stop()
                    guard !Task.isCancelled, !shuttingDown else { break }
                    try engine.start()
                    hasLaunched = true
                    let snapshot: MonitorState = try await request("/api/state", timeout: 60)
                    if let expected = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String,
                       snapshot.version != expected {
                        throw EngineTransportError.failed("Bundled monitoring engine version does not match this app. Rebuild or reinstall the app.")
                    }
                    state = snapshot
                    let payload: RegionsPayload = try await request("/api/regions")
                    regions = payload.regions
                    if !regions.contains(where: { $0.locale == selectedRegion }) { selectedRegion = "en_AU" }
                    try await loadCatalog(locale: selectedRegion)
                    guard !Task.isCancelled, !shuttingDown else { break }
                    isReady = true
                    isRecovering = false
                    engineStatus = "Ready"
                    errorMessage = nil
                    failedStarts = 0
                    pollFailures = 0
                    resumingMonitoring = desiredRunning && !state.running && !state.targets.isEmpty
                    backendResumeAt = Date(timeIntervalSince1970: (state.nextAllowedStartAt ?? 0) / 1000)
                } catch {
                    if Task.isCancelled || shuttingDown { break }
                    isReady = false
                    failedStarts += 1
                    isRecovering = failedStarts < 3
                    engineStatus = failedStarts < 3 ? "Restarting monitoring engine…" : "Monitoring engine unavailable"
                    errorMessage = error.localizedDescription
                    let delays: [UInt64] = [1, 3, 10, 30, 60]
                    let delay = delays[min(failedStarts - 1, delays.count - 1)]
                    do { try await Task.sleep(nanoseconds: delay * 1_000_000_000) } catch { break }
                    continue
                }
            }
            await refresh()
            if isReady {
                await resumeIfReady()
                await updateLanguagePreference()
            }
            do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { break }
        }
    }

    private func engineFailed() {
        guard !shuttingDown else { return }
        isReady = false
        isRecovering = true
        resumingMonitoring = desiredRunning
        engineStatus = "Restarting monitoring engine…"
        errorMessage = "The monitoring engine stopped unexpectedly. Restarting automatically."
    }

    private func request<T: Decodable>(_ path: String, method: String = "GET", body: [String: Any]? = nil,
                                      query: [URLQueryItem] = [], timeout: TimeInterval = 8) async throws -> T {
        var components = URLComponents()
        components.path = path
        components.queryItems = query.isEmpty ? nil : query
        let route = components.percentEncodedPath + (components.percentEncodedQuery.map { "?" + $0 } ?? "")
        let data = try await engine.request(path: route, method: method, body: body, timeout: timeout)
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { throw EngineTransportError.invalidResponse }
    }

    func refresh() async {
        guard !refreshing, !shuttingDown, isReady else { return }
        refreshing = true
        defer { refreshing = false }
        do {
            let snapshot: MonitorState = try await request("/api/state", timeout: 5)
            let refreshFinished = state.catalogRefreshing && !snapshot.catalogRefreshing
            state = snapshot
            pollFailures = 0
            if !isBusy && !resumingMonitoring { desiredRunning = snapshot.running }
            if snapshot.targets.isEmpty { desiredRunning = false; resumingMonitoring = false }
            if refreshFinished { try await loadCatalog(locale: selectedRegion) }
        } catch EngineTransportError.remote(_, let message) {
            errorMessage = message
        } catch {
            guard !shuttingDown else { return }
            pollFailures += 1
            if !engine.isRunning || pollFailures >= 3 { engineFailed() }
        }
    }

    private func resumeIfReady() async {
        guard resumingMonitoring, desiredRunning, isReady, !isBusy,
              (backendResumeAt ?? .distantPast) <= Date() else { return }
        do {
            let _: APIAcknowledgement = try await request("/api/monitor", method: "POST", body: ["running": true])
            resumingMonitoring = false
            backendResumeAt = nil
            await refresh()
        } catch EngineTransportError.remote(429, _) {
            backendResumeAt = Date().addingTimeInterval(3)
        } catch EngineTransportError.remote(_, let message) {
            resumingMonitoring = false
            desiredRunning = false
            errorMessage = message
        } catch { engineFailed() }
    }

    @discardableResult
    private func mutate(_ path: String, method: String = "POST", body: [String: Any], timeout: TimeInterval = 15) async -> Bool {
        guard isReady, !shuttingDown else { return false }
        do {
            let _: APIAcknowledgement = try await request(path, method: method, body: body, timeout: timeout)
            await refresh()
            return true
        } catch EngineTransportError.remote(_, let message) {
            errorMessage = message
            return false
        } catch {
            // Never automatically replay mutations: a timed-out notification may
            // already have been accepted by its provider.
            errorMessage = error.localizedDescription
            if !engine.isRunning { engineFailed() }
            return false
        }
    }

    func setRunning(_ running: Bool) async {
        guard !isBusy, isReady else { return }
        desiredRunning = running
        resumingMonitoring = false
        isBusy = true; defer { isBusy = false }
        errorMessage = nil
        do {
            let _: APIAcknowledgement = try await request("/api/monitor", method: "POST", body: ["running": running])
            await refresh()
        } catch EngineTransportError.remote(429, _) where running {
            resumingMonitoring = true
            backendResumeAt = Date(timeIntervalSince1970: (state.nextAllowedStartAt ?? 0) / 1000)
        } catch EngineTransportError.remote(_, let message) {
            desiredRunning = state.running
            errorMessage = message
        } catch { engineFailed() }
    }

    func addTargets(storeNumbers: [String], partNumbers: [String]) async -> Bool {
        guard !isBusy, !isCatalogLoading else { return false }
        isBusy = true; defer { isBusy = false }
        errorMessage = nil
        return await mutate("/api/targets", body: ["locale": selectedRegion, "storeNumbers": storeNumbers, "partNumbers": partNumbers])
    }

    func removeTargets(keys: [String]) async {
        guard !isBusy else { return }
        isBusy = true; defer { isBusy = false }
        errorMessage = nil
        for key in keys { if !(await mutate("/api/targets/remove", body: ["key": key])) { break } }
    }

    func saveSettings(intervalSeconds: Int, pushoverEnabled: Bool, barkEnabled: Bool,
                      pushoverToken: String, pushoverUser: String, barkURL: String) async -> Bool {
        guard !isBusy else { return false }
        isBusy = true; defer { isBusy = false }
        errorMessage = nil; successMessage = nil
        var credentials: [String: Any] = [:]
        for (key, value) in [("pushoverToken", pushoverToken), ("pushoverUser", pushoverUser), ("barkUrl", barkURL)] {
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { credentials[key] = trimmed }
        }
        if !credentials.isEmpty {
            guard await mutate("/api/credentials", method: "PUT", body: credentials, timeout: 90) else { return false }
        }
        let saved = await mutate("/api/settings", method: "PUT", body: ["intervalSeconds": intervalSeconds, "pushoverEnabled": pushoverEnabled, "barkEnabled": barkEnabled, "notificationLanguage": L10n.resolvedLanguage()])
        if saved { successMessage = "Settings saved." }
        return saved
    }

    func canTestNotification(channel: String, pushoverToken: String = "", pushoverUser: String = "", barkURL: String = "") -> Bool {
        guard isReady, !isBusy else { return false }
        switch channel {
        case "pushover":
            return state.settings.pushoverConfigured ||
                (!pushoverToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
                 !pushoverUser.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        case "bark":
            return state.settings.barkConfigured || !barkURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        default:
            return false
        }
    }

    func testNotification(channel: String, pushoverToken: String = "", pushoverUser: String = "", barkURL: String = "") async -> Bool {
        guard canTestNotification(channel: channel, pushoverToken: pushoverToken, pushoverUser: pushoverUser, barkURL: barkURL) else { return false }
        isBusy = true; defer { isBusy = false }
        errorMessage = nil; successMessage = nil
        var body: [String: Any] = ["channel": channel]
        let fields = channel == "pushover" ? [("pushoverToken", pushoverToken), ("pushoverUser", pushoverUser)] : [("barkUrl", barkURL)]
        for (key, value) in fields {
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { body[key] = trimmed }
        }
        let sent = await mutate("/api/notifications/test", body: body, timeout: 20)
        if sent { successMessage = "Test accepted by \(channel == "pushover" ? "Pushover" : "Bark"). Check your device." }
        return sent
    }

    func refreshCatalog(category: String) async {
        guard !isBusy else { return }
        isBusy = true; defer { isBusy = false }
        errorMessage = nil
        _ = await mutate("/api/catalog/refresh", body: ["locale": selectedRegion, "category": category])
    }

    private func loadCatalog(locale: String) async throws {
        catalogGeneration += 1
        let generation = catalogGeneration
        isCatalogLoading = true
        defer { if generation == catalogGeneration { isCatalogLoading = false } }
        let result: CatalogPayload = try await request("/api/catalog", query: [URLQueryItem(name: "locale", value: locale)], timeout: 15)
        guard generation == catalogGeneration, selectedRegion == locale else { return }
        catalog = result
    }

    func selectRegion(_ locale: String) async {
        guard regions.contains(where: { $0.locale == locale }) else { return }
        selectedRegion = locale
        UserDefaults.standard.set(locale, forKey: "selectedRegion")
        catalog = .empty
        errorMessage = nil
        do { try await loadCatalog(locale: locale) }
        catch { if selectedRegion == locale { errorMessage = error.localizedDescription } }
    }

    func updateLanguagePreference() async {
        guard isReady, !isBusy, !languageSyncing, state.notificationLanguage != L10n.resolvedLanguage() else { return }
        languageSyncing = true
        defer { languageSyncing = false }
        _ = await mutate("/api/language", method: "PUT", body: ["language": L10n.resolvedLanguage()])
    }

    func shutdown() async {
        shuttingDown = true
        desiredRunning = false
        lifecycleTask?.cancel()
        lifecycleTask = nil
        await engine.stop()
        isReady = false
        isRecovering = false
    }
}
