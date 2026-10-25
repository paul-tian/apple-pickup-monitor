import Foundation
import Darwin

struct TestFailure: Error { let message: String }
func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw TestFailure(message: message) }
}

// A reduced real /api/state response captured during the first-round bug report.
// Contains no session token or notification credentials.
let capturedState = #"""
{
  "catalogRefreshing": false, "healthy": null, "intervalSeconds": 60,
  "nextCheckAt": null, "notice": null, "notifications": [], "paced": false,
  "running": true,
  "settings": {"barkConfigured": false, "barkEnabled": false,
               "pushoverConfigured": false, "pushoverEnabled": false},
  "targets": [{
    "availability": {"kind": "unknown", "reason": "not_yet_checked"},
    "availabilityDetail": "Waiting for the first check.",
    "consecutiveFailures": 0, "key": "en_AU|R238|MK2D4X/A", "lastCheckedMs": null,
    "target": {"locale": "en_AU", "partNumber": "MK2D4X/A", "pickupLocation": "Sydney",
               "productName": "iPhone Duo 256GB Star White", "storeNumber": "R238",
               "storeTitle": "New South Wales-Sydney"}
  }],
  "version": "0.1.0"
}
"""#

func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(type, from: Data(json.utf8))
}

do {
    // Keep label expectations deterministic without changing persisted preferences.
    let previousArguments = UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)
    var arguments = previousArguments
    arguments["uiLanguage"] = "en"
    UserDefaults.standard.setVolatileDomain(arguments, forName: UserDefaults.argumentDomain)
    defer { UserDefaults.standard.setVolatileDomain(previousArguments, forName: UserDefaults.argumentDomain) }

    let state = try decode(MonitorState.self, capturedState)
    try expect(state.running && state.targets.count == 1, "Captured state did not decode")
    try expect(state.targets[0].id == "en_AU|R238|MK2D4X/A", "Monitoring target identity changed")
    try expect(state.targets[0].availability.isUnknown, "Pending result must remain unknown")
    try expect(state.targets[0].availability.label == "Waiting", "Pending label changed")
    try expect(state.nextAllowedStartAt == nil && state.progress == nil, "Older states must support absent optional fields")

    let labels: [(String, String?, String)] = [
        ("in_stock", nil, "Available"), ("out_of_stock", nil, "Unavailable"),
        ("unknown", "pickup_not_open", "Pickup not open"),
        ("unknown", "blocked", "Unknown"), ("unknown", "transport", "Unknown")
    ]
    for (kind, reason, label) in labels {
        let availability = Availability(kind: kind, reason: reason)
        try expect(availability.label == label, "Availability label mismatch for \(kind)")
        try expect(availability.isAvailable == (kind == "in_stock"), "Only confirmed stock may be available")
    }

    let progress = try decode(RoundProgress.self, #"{"completedStores":3,"totalStores":8,"activeLocation":"Sydney","startedAt":1792946759646,"phase":"waiting","waitUntil":1792946819646}"#)
    try expect(progress.completedStores == 3 && progress.waitUntil != nil, "Progress did not decode")
    let notification = try decode(NotificationRecord.self, #"{"id":1,"createdAt":1792946759646,"title":"Test","message":"Fixture","channel":"pushover","status":"sent","error":null}"#)
    try expect(notification.error == nil && notification.status == "sent", "Notification did not decode")

    let regions = try decode(RegionsPayload.self, #"{"regions":[{"locale":"en_AU","title":"Australia","baseUrl":"https://www.apple.com/au"},{"locale":"zh_CN","title":"中国大陆","baseUrl":"https://www.apple.com.cn"}]}"#)
    try expect(regions.regions.count == 2 && regions.regions[1].id == "zh_CN", "Regional metadata did not decode")
    try expect(regions.regions[1].baseUrl == "https://www.apple.com.cn", "Chinese storefront must retain its domain")
    let oldCatalog = try decode(CatalogPayload.self, #"{"stores":[],"products":[]}"#)
    let regionalCatalog = try decode(CatalogPayload.self, #"{"locale":"ja_JP","stores":[],"products":[]}"#)
    try expect(oldCatalog.locale == nil && regionalCatalog.locale == "ja_JP", "Regional catalogue compatibility changed")
    try expect(L10n.resolvedLanguage("en") == "en", "English language selection changed")
    try expect(L10n.resolvedLanguage("zh-Hans") == "zh-Hans", "Simplified Chinese language selection changed")
    try expect(L10n.text("Pause", language: "en") == "Pause", "English resource missing")
    try expect(L10n.text("Pause", language: "zh-Hans") == "暂停", "Simplified Chinese resource missing")
    try expect(L10n.text("untranslated-test-key", language: "zh-Hans") == "untranslated-test-key", "Missing keys must fall back safely")
    try expect(L10n.format("%d test count", 2, language: "en") == "2 test count", "Localized formatting changed")
    try expect(L10n.regionName("ja_JP", language: "en") == "Japan", "Region names should follow interface language")
    if Bundle.main.path(forResource: "zh-Hant", ofType: "lproj") != nil {
        try expect(L10n.text("Pause", language: "zh-Hant") == "暫停", "Traditional Chinese resource missing")
    }

    print("Native model tests passed: state, availability, progress, notifications, regions and localization.")
} catch let failure as TestFailure {
    fputs("Native model test failed: \(failure.message)\n", stderr)
    exit(1)
} catch {
    fputs("Native model test failed: \(error)\n", stderr)
    exit(1)
}
