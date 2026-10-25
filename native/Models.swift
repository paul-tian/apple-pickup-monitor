import Foundation

struct MonitorState: Decodable {
    var version: String
    var running: Bool
    var intervalSeconds: Int
    var nextCheckAt: Double?
    var paced: Bool
    var healthy: Bool?
    var notice: String?
    var catalogRefreshing: Bool
    var targets: [WatchRow]
    var notifications: [NotificationRecord]
    var settings: NotificationSettings
    var progress: RoundProgress?
    var nextAllowedStartAt: Double?
    var notificationLanguage: String? = nil

    static let empty = MonitorState(version: "", running: false, intervalSeconds: 60,
        nextCheckAt: nil, paced: false, healthy: nil, notice: nil, catalogRefreshing: false,
        targets: [], notifications: [], settings: .empty, progress: nil, nextAllowedStartAt: nil)
}

struct WatchRow: Decodable, Identifiable {
    var id: String { key }
    let key: String
    let target: WatchTarget
    let availability: Availability
    let lastCheckedMs: Double?
    let availabilityDetail: String
    let checkPhase: String?
    let consecutiveFailures: Int
}

struct WatchTarget: Decodable {
    let locale: String
    let storeNumber: String
    let storeTitle: String
    let partNumber: String
    let productName: String
}

struct Availability: Decodable {
    let kind: String
    let reason: String?
    var isAvailable: Bool { kind == "in_stock" }
    var isUnknown: Bool { kind == "unknown" }
    var label: String {
        switch kind {
        case "in_stock": return L10n.text("Available")
        case "out_of_stock": return L10n.text("Unavailable")
        default:
            switch reason {
            case "not_yet_checked": return L10n.text("Waiting")
            case "pickup_not_open": return L10n.text("Pickup not open")
            default: return L10n.text("Unknown")
            }
        }
    }
}

struct RoundProgress: Decodable {
    let completedStores: Int
    let totalStores: Int
    let activeLocation: String?
    let startedAt: Double
    let phase: String
    let waitUntil: Double?
}

struct NotificationSettings: Decodable {
    var pushoverEnabled: Bool
    var barkEnabled: Bool
    var pushoverConfigured: Bool
    var barkConfigured: Bool
    static let empty = NotificationSettings(pushoverEnabled: false, barkEnabled: false,
        pushoverConfigured: false, barkConfigured: false)
}

struct NotificationRecord: Decodable, Identifiable {
    let id: Int
    let createdAt: Double
    let title: String
    let message: String
    let channel: String
    let status: String
    let error: String?
}

struct CatalogPayload: Decodable {
    var locale: String? = nil
    var stores: [StoreItem]
    var products: [ProductItem]
    init(locale: String? = nil, stores: [StoreItem], products: [ProductItem]) {
        self.locale = locale; self.stores = stores; self.products = products
    }
    static let empty = CatalogPayload(stores: [], products: [])
}

struct RegionItem: Decodable, Identifiable {
    var id: String { locale }
    let locale: String
    let title: String
    let baseUrl: String
    var displayName: String { L10n.regionName(locale) }
}

struct RegionsPayload: Decodable { let regions: [RegionItem] }

struct StoreItem: Decodable, Identifiable, Hashable {
    var id: String { number }
    let number: String
    let name: String
    let title: String
}

struct ProductItem: Decodable, Identifiable, Hashable {
    var id: String { partNumber }
    let partNumber: String
    let category: String
    let family: String
    let capacity: String
    let color: String
    let title: String
    let companionPart: String?

    var familyLabel: String {
        guard !capacity.isEmpty, let range = title.range(of: capacity) else { return title }
        return String(title[..<range.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct APIAcknowledgement: Decodable { let ok: Bool }
