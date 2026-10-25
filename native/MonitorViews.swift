import SwiftUI
import AppKit

/// A compact native interpretation of Apple Pickup Watcher's desktop layout.
struct MonitorContentView: View {
    @EnvironmentObject var model: MonitorModel
    @State private var category = "iphone"
    @State private var stores = Set<String>()
    @State private var families = Set<String>()
    @State private var capacities = Set<String>()
    @State private var colors = Set<String>()
    @State private var parts = Set<String>()
    @State private var selectedRows = Set<String>()
    @AppStorage("native.settingsExpanded") private var settingsExpanded = false
    @AppStorage("uiLanguage") private var uiLanguage = "system"
    @State private var interval = 60
    @State private var pushoverEnabled = false
    @State private var barkEnabled = false
    @State private var pushoverToken = ""
    @State private var pushoverUser = ""
    @State private var barkURL = ""
    @State private var settingsLoaded = false

    private var products: [ProductItem] { model.catalog.products.filter { $0.category == category } }
    private var chosenProducts: [ProductItem] {
        if category == "iphone" {
            return products.filter { families.contains($0.family) && capacities.contains($0.capacity) && colors.contains($0.color) }
        }
        return products.filter { parts.contains($0.partNumber) }
    }
    private var familyOptions: [SelectionOption] {
        let grouped = Dictionary(grouping: products, by: \.family)
        return grouped.map { key, values in
            let product = values[0]
            return SelectionOption(id: key, title: product.familyLabel, subtitle: nil)
        }.sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
    }
    private var capacityOptions: [SelectionOption] {
        Set(products.filter { families.contains($0.family) }.map(\.capacity))
            .sorted { capacityOrder($0) < capacityOrder($1) }
            .map { SelectionOption(id: $0, title: $0.isEmpty ? L10n.text("Standard") : $0, subtitle: nil) }
    }
    private var colorOptions: [SelectionOption] {
        Set(products.filter { families.contains($0.family) && capacities.contains($0.capacity) }.map(\.color))
            .sorted().map { SelectionOption(id: $0, title: $0.isEmpty ? L10n.text("Standard") : $0, subtitle: nil) }
    }
    private var watchCount: Int { stores.count * chosenProducts.count }
    private var availableCount: Int { model.state.targets.filter { $0.availability.isAvailable }.count }
    private var unknownCount: Int { model.state.targets.filter { $0.availability.isUnknown }.count }
    private var unavailableCount: Int { model.state.targets.filter { $0.availability.kind == "out_of_stock" }.count }
    private var monitoringRequested: Bool { model.state.running || model.resumingMonitoring }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let message = model.errorMessage, !message.isEmpty {
                notice(message, symbol: "exclamationmark.triangle.fill", color: .orange)
            } else if let message = model.state.notice, !message.isEmpty {
                notice(message, symbol: "info.circle.fill", color: .orange)
            } else if let message = model.successMessage, !message.isEmpty {
                notice(message, symbol: "checkmark.circle.fill", color: .green)
            }
            addPanel
            settingsPanel
            summary
            watchTable
            if !settingsExpanded { activityPanel }
            footer
        }
        .padding(18)
        .frame(minWidth: 1000, minHeight: 740)
        .background(Color(nsColor: .windowBackgroundColor))
        .environment(\.locale, L10n.locale(for: uiLanguage))
        .onAppear { loadSettingsIfReady() }
        .onChange(of: model.isReady) { _, ready in
            if ready { loadSettingsIfReady() }
        }
        .onChange(of: uiLanguage) {
            Task { await model.updateLanguagePreference() }
        }
        .onChange(of: model.selectedRegion) {
            stores.removeAll(); families.removeAll(); capacities.removeAll(); colors.removeAll(); parts.removeAll()
        }
        .onChange(of: category) {
            families.removeAll(); capacities.removeAll(); colors.removeAll(); parts.removeAll()
        }
        .onChange(of: families) {
            capacities.formIntersection(Set(capacityOptions.map(\.id)))
            colors.formIntersection(Set(colorOptions.map(\.id)))
        }
        .onChange(of: capacities) { colors.formIntersection(Set(colorOptions.map(\.id))) }
        .onChange(of: model.state.targets.map(\.key)) { _, keys in selectedRows.formIntersection(Set(keys)) }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 3) {
                Text(L10n.text("Apple Pickup Monitor")).font(.system(size: 23, weight: .semibold))
                Text(L10n.text("Monitor Apple Store pickup availability. Get notified when a product is available for pickup."))
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            Spacer()
            HStack(spacing: 6) {
                Circle().fill(!model.isReady ? Color.orange : model.state.running ? Color.green : Color.secondary)
                    .frame(width: 7, height: 7)
                Text(L10n.text(!model.isReady ? model.engineStatus : model.resumingMonitoring ? "Resuming shortly" : model.state.running ? "Monitoring" : "Paused"))
                    .foregroundStyle(.secondary).font(.subheadline)
            }
            Button {
                Task { await model.setRunning(!monitoringRequested) }
            } label: {
                Label(L10n.text(monitoringRequested ? "Pause" : "Start monitoring"), systemImage: monitoringRequested ? "pause.fill" : "play.fill")
                    .frame(minWidth: 110)
            }
            .buttonStyle(.borderedProminent).controlSize(.large)
            .disabled(!model.isReady || model.isBusy || (!model.state.running && model.state.targets.isEmpty))
            .keyboardShortcut("r", modifiers: [.command])
            .help(L10n.text(model.state.running ? "Pause stock checks" : "Start stock checks (⌘R)"))
        }
    }

    private var addPanel: some View {
        NativePanel {
            HStack {
                Text(L10n.text("Add monitoring targets")).fontWeight(.semibold)
                Text(L10n.text("Choose multiple stores and configurations to add them together."))
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
            }
        } content: {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .bottom, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.text("Region")).font(.caption).foregroundStyle(.secondary)
                        Picker(L10n.text("Region"), selection: Binding(
                            get: { model.selectedRegion },
                            set: { value in Task { await model.selectRegion(value) } }
                        )) {
                            ForEach(model.regions, id: \.locale) { region in
                                Text(L10n.regionName(region.locale, language: uiLanguage)).tag(region.locale)
                            }
                        }.labelsHidden().frame(height: 28)
                    }.frame(width: 200)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.text("Category")).font(.caption).foregroundStyle(.secondary)
                        Picker(L10n.text("Category"), selection: $category) {
                            Text(L10n.text("iPhone")).tag("iphone")
                            Text(L10n.text("iPad")).tag("ipad")
                            Text(L10n.text("Mac")).tag("mac")
                            Text(L10n.text("Apple Watch")).tag("watch")
                        }.labelsHidden().frame(height: 28)
                    }.frame(width: 155)
                    MultiSelectionField(title: "Apple Stores", placeholder: "Choose stores", options: model.catalog.stores.map {
                        SelectionOption(id: $0.number, title: $0.name, subtitle: $0.title)
                    }, selection: $stores)
                    .frame(minWidth: 230, maxWidth: .infinity).disabled(model.isCatalogLoading)
                    Button {
                        Task { await model.refreshCatalog(category: category) }
                    } label: {
                        if model.state.catalogRefreshing || model.isCatalogLoading { ProgressView().controlSize(.small).frame(width: 16, height: 16) }
                        else { Image(systemName: "arrow.clockwise") }
                    }.frame(width: 32, height: 28)
                    .disabled(model.state.catalogRefreshing || model.isCatalogLoading || !model.isReady || model.isBusy)
                    .help(L10n.text("Refresh this category from the selected Apple region"))
                    .accessibilityLabel(L10n.text("Refresh product catalogue"))
                }
                HStack(alignment: .bottom, spacing: 12) {
                    if category == "iphone" {
                        MultiSelectionField(title: "Model", placeholder: "Choose models", options: familyOptions, selection: $families)
                            .frame(minWidth: 210, maxWidth: .infinity)
                        MultiSelectionField(title: "Capacity", placeholder: "Choose capacities", options: capacityOptions, selection: $capacities)
                            .frame(width: 160).disabled(families.isEmpty)
                        MultiSelectionField(title: "Colour", placeholder: "Choose colours", options: colorOptions, selection: $colors)
                            .frame(minWidth: 180, maxWidth: .infinity).disabled(capacities.isEmpty)
                    } else {
                        MultiSelectionField(title: "Configuration", placeholder: "Choose configurations", options: products.map {
                            SelectionOption(id: $0.partNumber, title: $0.title, subtitle: $0.partNumber)
                        }, selection: $parts).frame(maxWidth: .infinity)
                    }
                    VStack(alignment: .trailing, spacing: 4) {
                        Text(watchCount == 0 ? " " : L10n.format("%d configurations × %d stores", chosenProducts.count, stores.count))
                            .font(.caption2).foregroundStyle(.secondary)
                        Button {
                            let selected = chosenProducts.map(\.partNumber)
                            Task {
                                if await model.addTargets(storeNumbers: stores.sorted(), partNumbers: selected) {
                                    families.removeAll(); capacities.removeAll(); colors.removeAll(); parts.removeAll()
                                }
                            }
                        } label: {
                            Label(watchCount > 0 ? L10n.format(watchCount == 1 ? "Add %d monitoring target" : "Add %d monitoring targets", watchCount) : L10n.text("Add monitoring targets"), systemImage: "plus")
                                .frame(minWidth: 115)
                        }.controlSize(.large).buttonStyle(.bordered)
                        .disabled(watchCount == 0 || model.isBusy || model.isCatalogLoading || !model.isReady)
                    }
                }.disabled(model.isCatalogLoading)
                if model.isCatalogLoading {
                    Text(L10n.text("Loading regional catalogue…")).font(.caption).foregroundStyle(.secondary)
                }
            }.disabled(!model.isReady)
        }
    }

    private var settingsPanel: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { settingsExpanded.toggle() }
            } label: {
                HStack(spacing: 12) {
                    Text(L10n.text("Monitoring & notifications")).fontWeight(.semibold).foregroundStyle(.primary)
                    Text(settingsSummary).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    Spacer()
                    Image(systemName: settingsExpanded ? "chevron.up" : "chevron.down").foregroundStyle(.secondary)
                }.padding(.horizontal, 12).padding(.vertical, 10).contentShape(Rectangle())
            }.buttonStyle(.plain)
            if settingsExpanded {
                Divider()
                VStack(alignment: .trailing, spacing: 12) {
                    HStack(alignment: .top, spacing: 20) {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(L10n.text("Interface language")).font(.caption).foregroundStyle(.secondary)
                            Picker(L10n.text("Interface language"), selection: $uiLanguage) {
                                Text(L10n.text("System default")).tag("system")
                                Text("English").tag("en")
                                Text("简体中文").tag("zh-Hans")
                            }.labelsHidden()

                            Text(L10n.text("Polling interval")).font(.caption).foregroundStyle(.secondary)
                            HStack {
                                TextField(L10n.text("Seconds"), value: $interval, format: .number)
                                    .textFieldStyle(.roundedBorder).frame(width: 66)
                                    .accessibilityLabel(L10n.text("Polling interval in seconds"))
                                Text(L10n.text("seconds")).foregroundStyle(.secondary)
                            }
                            Text(L10n.text("Minimum 60 seconds.\nActual checks respect Apple's request limits."))
                                .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        }.frame(width: 190, alignment: .leading)
                        VStack(alignment: .leading, spacing: 7) {
                            HStack {
                                Toggle(L10n.text("Pushover"), isOn: $pushoverEnabled).toggleStyle(.checkbox)
                                Spacer()
                                if model.state.settings.pushoverConfigured { Label(L10n.text("Keys saved"), systemImage: "checkmark.shield").font(.caption2).foregroundStyle(.secondary) }
                            }
                            SecureField(L10n.text("Application API token"), text: $pushoverToken).textFieldStyle(.roundedBorder)
                            SecureField(L10n.text("User or group key"), text: $pushoverUser).textFieldStyle(.roundedBorder)
                            HStack {
                                Text(L10n.text("Leave blank to keep saved keys.")).font(.caption2).foregroundStyle(.secondary)
                                Spacer()
                                Button(L10n.text("Send test")) {
                                    Task { _ = await model.testNotification(channel: "pushover", pushoverToken: pushoverToken, pushoverUser: pushoverUser) }
                                }
                                    .accessibilityLabel(L10n.text("Send Pushover test"))
                                    .disabled(!model.canTestNotification(channel: "pushover", pushoverToken: pushoverToken, pushoverUser: pushoverUser))
                                    .help(L10n.text("Test without saving. Blank fields use saved credentials."))
                            }
                        }.frame(maxWidth: .infinity)
                        VStack(alignment: .leading, spacing: 7) {
                            HStack {
                                Toggle(L10n.text("Bark"), isOn: $barkEnabled).toggleStyle(.checkbox)
                                Spacer()
                                if model.state.settings.barkConfigured { Label(L10n.text("URL saved"), systemImage: "checkmark.shield").font(.caption2).foregroundStyle(.secondary) }
                            }
                            SecureField(L10n.text("Bark push URL (https://…)"), text: $barkURL).textFieldStyle(.roundedBorder)
                            Text(L10n.text("Notification credentials are stored in macOS Keychain."))
                                .font(.caption2).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            HStack {
                                Spacer()
                                Button(L10n.text("Send test")) {
                                    Task { _ = await model.testNotification(channel: "bark", barkURL: barkURL) }
                                }
                                    .accessibilityLabel(L10n.text("Send Bark test"))
                                    .disabled(!model.canTestNotification(channel: "bark", barkURL: barkURL))
                                    .help(L10n.text("Test without saving. Blank fields use saved credentials."))
                            }
                        }.frame(maxWidth: .infinity)
                    }
                    Button(L10n.text("Save settings")) { saveSettings() }
                        .buttonStyle(.borderedProminent)
                        .disabled(model.isBusy || !model.isReady || !(60...3600).contains(interval))
                }.padding(12).fixedSize(horizontal: false, vertical: true)
            }
        }
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.primary.opacity(0.1)))
    }

    private var summary: some View {
        HStack(spacing: 8) {
            SummaryBadge(title: "Monitoring targets", count: model.state.targets.count, color: .primary)
            SummaryBadge(title: "Available", count: availableCount, color: .green)
            SummaryBadge(title: "Unavailable", count: unavailableCount, color: .secondary)
            if unknownCount > 0 { SummaryBadge(title: "Unconfirmed", count: unknownCount, color: .orange) }
            Spacer()
            if !selectedRows.isEmpty {
                Button(role: .destructive) {
                    let keys = Array(selectedRows)
                    Task { await model.removeTargets(keys: keys); selectedRows.removeAll() }
                } label: { Label(L10n.format("Remove %d selected", selectedRows.count), systemImage: "trash") }
                    .disabled(model.isBusy || !model.isReady)
            }
        }
    }

    private var watchTable: some View {
        ZStack {
            Table(model.state.targets, selection: $selectedRows) {
                TableColumn(L10n.text("Status")) { row in NativeStatusBadge(row: row) }.width(min: 105, ideal: 110, max: 120)
                TableColumn(L10n.text("Apple Store")) { row in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(storeName(row.target)).fontWeight(.medium)
                        Text(L10n.regionName(row.target.locale, language: uiLanguage) + " · " + row.target.storeNumber).font(.caption2).foregroundStyle(.tertiary)
                    }.padding(.vertical, 5)
                }.width(min: 130, ideal: 170, max: 230)
                TableColumn(L10n.text("Configuration")) { row in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(row.target.productName).foregroundColor(row.availability.isAvailable ? .green : .primary)
                        Text(row.target.partNumber).font(.caption2).foregroundStyle(.secondary)
                    }.padding(.vertical, 5).help(localizedAvailabilityDetail(row))
                }.width(min: 250, ideal: 390)
                TableColumn(L10n.text("Last checked")) { row in
                    if let timestamp = row.lastCheckedMs {
                        Text(Date(timeIntervalSince1970: timestamp / 1000), style: .time)
                            .monospacedDigit().foregroundStyle(.secondary)
                    } else { Text(L10n.text("—")).foregroundStyle(.tertiary) }
                }.width(95)
                TableColumn("") { row in
                    Button(role: .destructive) { Task { await model.removeTargets(keys: [row.key]) } } label: {
                        Image(systemName: "trash")
                    }.buttonStyle(.borderless)
                        .disabled(model.isBusy || !model.isReady)
                        .help(L10n.text("Remove this monitoring target"))
                        .accessibilityLabel(L10n.format("Remove %@ at %@", row.target.productName, row.target.storeTitle))
                }.width(28)
            }.tableStyle(.inset)
                .onDeleteCommand {
                    guard !selectedRows.isEmpty, !model.isBusy, model.isReady else { return }
                    let keys = Array(selectedRows)
                    Task { await model.removeTargets(keys: keys); selectedRows.removeAll() }
                }
            if model.state.targets.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "bag.badge.plus").font(.system(size: 28)).foregroundStyle(.secondary)
                    Text(L10n.text("Choose what you're waiting for")).font(.headline)
                    Text(L10n.text("Add a configuration and one or more stores above.")).foregroundStyle(.secondary)
                }.allowsHitTesting(false)
            }
        }
        .frame(minHeight: 150, maxHeight: .infinity)
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.primary.opacity(0.1)))
    }

    private var activityPanel: some View {
        NativePanel {
            HStack {
                Text(L10n.text("Recent notifications")).fontWeight(.semibold)
                Spacer()
                Text(L10n.text("Pushover · Bark")).font(.caption).foregroundStyle(.secondary)
            }
        } content: {
            if model.state.notifications.isEmpty {
                HStack(spacing: 9) {
                    Image(systemName: "bell").foregroundStyle(.tertiary)
                    Text(L10n.text("Alerts and test notifications will appear here.")).foregroundStyle(.secondary)
                    Spacer()
                    Button(L10n.text("Set up notifications")) { settingsExpanded = true }
                        .buttonStyle(.link)
                }.font(.caption).padding(.vertical, 4)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 7) {
                        ForEach(model.state.notifications.prefix(15)) { item in
                            HStack(alignment: .top, spacing: 10) {
                                Text(Date(timeIntervalSince1970: item.createdAt / 1000), style: .time)
                                    .monospacedDigit().foregroundStyle(.secondary).frame(width: 80, alignment: .leading)
                                Image(systemName: item.status == "sent" ? "checkmark.circle" : item.status == "failed" ? "exclamationmark.circle" : "clock")
                                    .foregroundColor(item.status == "sent" ? .green : item.status == "failed" ? .orange : .secondary)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(L10n.text(item.title)).fontWeight(.medium)
                                    Text(L10n.text(item.error ?? item.message)).foregroundStyle(.secondary).lineLimit(2)
                                }
                                Spacer()
                                Text(item.channel.capitalized + " · " + L10n.notificationStatus(item.status)).foregroundStyle(.secondary)
                            }.font(.caption)
                        }
                    }
                }.frame(height: 88)
            }
        }
    }

    private var footer: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            HStack(spacing: 9) {
                if !model.isReady {
                    if model.isRecovering {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange)
                    }
                    Text(L10n.text(model.engineStatus)).foregroundStyle(.secondary)
                } else if model.resumingMonitoring {
                    Image(systemName: "clock").foregroundStyle(.secondary)
                    Text(L10n.text("Resuming monitoring at the next allowed request slot…")).foregroundStyle(.secondary)
                } else if model.state.running, let progress = model.state.progress {
                    ProgressView(value: Double(progress.completedStores), total: Double(max(progress.totalStores, 1)))
                        .frame(width: 80)
                    Text(L10n.format("%d / %d stores checked", progress.completedStores, progress.totalStores)).monospacedDigit()
                    Text(progressDetail(progress, now: context.date)).foregroundStyle(.secondary).lineLimit(1)
                } else if model.state.running {
                    Image(systemName: "clock").foregroundStyle(.secondary)
                    Text(nextCheckText(now: context.date)).monospacedDigit()
                    if model.state.paced { Text(L10n.text("Respecting Apple's request limits")).foregroundStyle(.secondary) }
                } else {
                    Image(systemName: "pause.circle").foregroundStyle(.secondary)
                    Text(L10n.text("Monitoring paused")).foregroundStyle(.secondary)
                }
                Spacer()
                Text(L10n.text("Keep your Mac awake. Closing the window keeps monitoring.")).foregroundStyle(.tertiary)
            }.font(.caption)
        }
    }

    private var settingsSummary: String {
        let channels = [model.state.settings.pushoverEnabled ? "Pushover" : nil, model.state.settings.barkEnabled ? "Bark" : nil].compactMap { $0 }
        return L10n.format("%ds minimum interval · %@", model.state.intervalSeconds, channels.isEmpty ? L10n.text("Notifications off") : channels.joined(separator: " + "))
    }
    private func loadSettingsIfReady() {
        guard model.isReady, !settingsLoaded else { return }
        interval = model.state.intervalSeconds
        pushoverEnabled = model.state.settings.pushoverEnabled
        barkEnabled = model.state.settings.barkEnabled
        settingsLoaded = true
    }
    private func saveSettings() {
        Task {
            if await model.saveSettings(intervalSeconds: interval, pushoverEnabled: pushoverEnabled, barkEnabled: barkEnabled, pushoverToken: pushoverToken, pushoverUser: pushoverUser, barkURL: barkURL) {
                pushoverToken = ""; pushoverUser = ""; barkURL = ""
            }
        }
    }
    private func storeName(_ target: WatchTarget) -> String {
        guard target.locale == model.selectedRegion else { return target.storeTitle }
        return model.catalog.stores.first { $0.number == target.storeNumber }?.name ?? target.storeTitle
    }
    private func nextCheckText(now: Date) -> String {
        guard let next = model.state.nextCheckAt else { return L10n.text("Starting next round…") }
        let seconds = max(0, Int(ceil(next / 1000 - now.timeIntervalSince1970)))
        if seconds == 0 { return L10n.text("Starting next round…") }
        return seconds >= 60 ? L10n.format("Next round in %dm %ds", seconds / 60, seconds % 60) : L10n.format("Next round in %ds", seconds)
    }
    private func progressDetail(_ progress: RoundProgress, now: Date) -> String {
        if let wait = progress.waitUntil, wait / 1000 > now.timeIntervalSince1970 {
            return L10n.format("Next request slot in %ds", Int(ceil(wait / 1000 - now.timeIntervalSince1970)))
        }
        if let location = progress.activeLocation { return L10n.format("Checking %@", location) }
        return L10n.text(progress.phase == "waiting" ? "Waiting for the next request slot" : "Checking Apple pickup availability")
    }
    private func capacityOrder(_ value: String) -> Double {
        let amount = Double(value.filter { $0.isNumber || $0 == "." }) ?? 0
        return value.localizedCaseInsensitiveContains("TB") ? amount * 1024 : amount
    }
    private func notice(_ text: String, symbol: String, color: Color) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: symbol).foregroundColor(color)
            Text(L10n.text(text)).textSelection(.enabled).lineLimit(3)
            Spacer(minLength: 0)
        }.font(.caption).padding(.horizontal, 12).padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(color.opacity(0.2)))
    }
}

private struct NativeStatusBadge: View {
    @AppStorage("uiLanguage") private var uiLanguage = "system"
    let row: WatchRow
    private var pending: Bool { row.availability.reason == "not_yet_checked" }
    private var color: Color { row.availability.isAvailable ? .green : row.availability.isUnknown && !pending ? .orange : .secondary }
    private var label: String {
        if pending, row.checkPhase == "queued" { return L10n.text("Queued") }
        if pending, row.checkPhase == "checking" { return L10n.text("Checking") }
        return L10n.text(row.availability.label)
    }
    var body: some View {
        Text(L10n.text(label, language: uiLanguage)).font(.caption).fontWeight(.medium)
            .foregroundColor(color).padding(.horizontal, 9).padding(.vertical, 4)
            .frame(minWidth: 83)
            .background(color.opacity(0.1), in: Capsule())
            .overlay(Capsule().stroke(color.opacity(row.availability.isAvailable ? 0.3 : 0.12)))
            .help(localizedAvailabilityDetail(row))
    }
}

private struct SummaryBadge: View {
    @AppStorage("uiLanguage") private var uiLanguage = "system"
    let title: String
    let count: Int
    let color: Color
    var body: some View {
        HStack(spacing: 6) {
            Text(L10n.text(title, language: uiLanguage)).font(.caption).foregroundStyle(.secondary)
            Text("\(count)").fontWeight(.semibold).monospacedDigit().foregroundColor(count > 0 ? color : .secondary)
        }.padding(.horizontal, 9).padding(.vertical, 5)
            .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.primary.opacity(0.08)))
    }
}

private struct NativePanel<Header: View, Content: View>: View {
    @ViewBuilder let header: () -> Header
    @ViewBuilder let content: () -> Content
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header().padding(.horizontal, 12).padding(.vertical, 9)
            Divider()
            content().padding(12)
        }.background(Color(nsColor: .controlBackgroundColor))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.primary.opacity(0.1)))
    }
}

private struct SelectionOption: Identifiable {
    let id: String
    let title: String
    let subtitle: String?
}

private struct MultiSelectionField: View {
    @AppStorage("uiLanguage") private var uiLanguage = "system"
    let title: String
    let placeholder: String
    let options: [SelectionOption]
    @Binding var selection: Set<String>
    @State private var expanded = false
    @State private var search = ""
    private var filtered: [SelectionOption] {
        guard !search.isEmpty else { return options }
        return options.filter { ($0.title + " " + ($0.subtitle ?? "") + " " + $0.id).localizedCaseInsensitiveContains(search) }
    }
    private var summary: String {
        if selection.isEmpty { return L10n.text(placeholder) }
        if selection.count == 1 { return options.first { selection.contains($0.id) }?.title ?? L10n.format("%d selected", 1) }
        return L10n.format("%d selected", selection.count)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L10n.text(title, language: uiLanguage)).font(.caption).foregroundStyle(.secondary)
            Button { expanded.toggle() } label: {
                HStack(spacing: 6) {
                    Text(summary).lineLimit(1).foregroundColor(selection.isEmpty ? .secondary : .primary)
                    Spacer(minLength: 4)
                    Image(systemName: "chevron.up.chevron.down").font(.system(size: 9)).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity).frame(height: 20)
            }.buttonStyle(.bordered)
                .accessibilityLabel(L10n.format("%@: %@", L10n.text(title), summary))
                .popover(isPresented: $expanded, arrowEdge: .bottom) {
                    VStack(spacing: 10) {
                        HStack {
                            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                            TextField(L10n.format("Search %@", L10n.text(title)), text: $search).textFieldStyle(.plain)
                        }.padding(8).background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 6))
                        HStack {
                            Button(L10n.text("Select visible")) { selection.formUnion(filtered.map(\.id)) }.buttonStyle(.link)
                            Spacer()
                            Button(L10n.text("Clear")) { selection.removeAll() }.buttonStyle(.link).disabled(selection.isEmpty)
                        }.font(.caption)
                        Divider()
                        ScrollView {
                            LazyVStack(alignment: .leading, spacing: 2) {
                                ForEach(filtered) { option in
                                    Toggle(isOn: Binding(get: { selection.contains(option.id) }, set: { checked in
                                        if checked { selection.insert(option.id) } else { selection.remove(option.id) }
                                    })) {
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(option.title).lineLimit(2)
                                            if let subtitle = option.subtitle, subtitle != option.title {
                                                Text(subtitle).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                                            }
                                        }.frame(maxWidth: .infinity, alignment: .leading)
                                    }.toggleStyle(.checkbox).padding(.vertical, 5).padding(.horizontal, 3)
                                }
                                if filtered.isEmpty {
                                    Text(L10n.text("No matching options")).foregroundStyle(.secondary).frame(maxWidth: .infinity).padding(.top, 25)
                                }
                            }
                        }.frame(height: 250)
                        Divider()
                        HStack {
                            Text(L10n.format("%d selected", selection.count)).font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Button(L10n.text("Done")) { expanded = false }.keyboardShortcut(.defaultAction)
                        }
                    }.padding(12).frame(width: 350)
                }
        }
    }
}


private func localizedAvailabilityDetail(_ row: WatchRow) -> String {
    if row.availability.reason == "not_yet_checked" {
        return L10n.text(row.checkPhase == "queued" ? "Queued for its next check slot." : row.checkPhase == "checking" ? "Checking this area with Apple." : "Waiting for the first check.")
    }
    switch row.availability.kind {
    case "in_stock": return L10n.text("Available for pickup at the last check.")
    case "out_of_stock": return L10n.text("Apple reports no pickup availability for this configuration.")
    default:
        switch row.availability.reason {
        case "blocked": return L10n.text("Apple blocked the request. Monitoring will cool down before retrying.")
        case "rate_limited": return L10n.text("Apple is limiting requests. Waiting before retrying.")
        case "schema_drift": return L10n.text("Apple returned an unexpected response. Availability is unknown.")
        case "transport": return L10n.text("The request could not complete. Check your internet connection.")
        case "pickup_not_open": return L10n.text("Apple has not opened pickup for this configuration yet.")
        default: return L10n.text(row.availabilityDetail)
        }
    }
}
