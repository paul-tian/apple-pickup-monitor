import Foundation

/// UI language is independent from an Apple Store's regional catalogue.
/// English source strings are stable keys; missing translations fall back to English.
enum L10n {
    static let defaultsKey = "uiLanguage"
    static let supportedLanguages = ["en", "zh-Hans"]

    static func resolvedLanguage(_ selection: String? = nil) -> String {
        let choice = selection ?? UserDefaults.standard.string(forKey: defaultsKey) ?? "system"
        if supportedLanguages.contains(choice) { return choice }
        guard choice == "system" else { return "en" }
        for preferred in Locale.preferredLanguages {
            if preferred.hasPrefix("zh") { return "zh-Hans" }
            if preferred.hasPrefix("en") { return "en" }
        }
        return "en"
    }

    static func locale(for selection: String? = nil) -> Locale {
        Locale(identifier: resolvedLanguage(selection))
    }

    static func text(_ key: String, language: String? = nil) -> String {
        let code = resolvedLanguage(language)
        let english = bundle(for: "en")?.localizedString(forKey: key, value: key, table: nil) ?? key
        return bundle(for: code)?.localizedString(forKey: key, value: english, table: nil) ?? english
    }

    static func format(_ key: String, _ arguments: CVarArg..., language: String? = nil) -> String {
        String(format: text(key, language: language), locale: locale(for: language), arguments: arguments)
    }

    static func regionName(_ regionLocale: String, language: String? = nil) -> String {
        if regionLocale == "zh_CN" { return text("China mainland", language: language) }
        let country = regionLocale.split(separator: "_").last.map(String.init) ?? regionLocale
        return locale(for: language).localizedString(forRegionCode: country) ?? regionLocale
    }

    static func notificationStatus(_ status: String) -> String {
        switch status {
        case "sent": return text("Sent")
        case "failed": return text("Failed")
        case "pending": return text("Pending")
        case "retrying": return text("Retrying")
        case "expired": return text("Expired")
        case "canceled": return text("Canceled")
        default: return status
        }
    }

    private static func bundle(for language: String) -> Bundle? {
        guard let path = Bundle.main.path(forResource: language, ofType: "lproj") else { return nil }
        return Bundle(path: path)
    }
}
