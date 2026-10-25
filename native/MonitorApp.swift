import SwiftUI
import AppKit

@main
@MainActor
struct PickupMonitorApp: App {
    @NSApplicationDelegateAdaptor(MonitorAppDelegate.self) private var delegate
    @StateObject private var model = MonitorModel.shared
    @AppStorage("uiLanguage") private var uiLanguage = "system"

    var body: some Scene {
        WindowGroup(L10n.text("Apple Pickup Monitor", language: uiLanguage)) {
            MonitorContentView()
                .environmentObject(model)
                .environment(\.locale, L10n.locale(for: uiLanguage))
                .frame(minWidth: 1000, minHeight: 740)
                .background(WindowCapture { window in
                    delegate.mainWindow = window
                    window.isReleasedWhenClosed = false
                    window.setFrameAutosaveName("PickupMonitorMainWindow")
                })
                .task { await model.start() }
        }
        .defaultSize(width: 1120, height: 850)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }

        MenuBarExtra(L10n.text("Pickup Monitor", language: uiLanguage), systemImage: model.isReady && model.state.running ? "bag.fill" : "bag") {
            Text(!model.isReady ? L10n.text(model.engineStatus) : model.resumingMonitoring ? L10n.text("Resuming monitoring…") : model.state.running ? L10n.format("Monitoring %d targets", model.state.targets.count) : L10n.text("Monitoring paused"))
            Button(L10n.text("Open Monitor")) { delegate.showWindow() }
                .keyboardShortcut("o")
            Button(L10n.text(model.state.running || model.resumingMonitoring ? "Pause Monitoring" : "Start Monitoring")) {
                Task { await model.setRunning(!(model.state.running || model.resumingMonitoring)) }
            }.disabled(!model.isReady || model.isBusy || model.state.targets.isEmpty)
            Divider()
            Button(L10n.text("Quit Apple Pickup Monitor")) { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        }
    }
}

@MainActor
final class MonitorAppDelegate: NSObject, NSApplicationDelegate {
    var mainWindow: NSWindow?
    private var terminating = false
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        showWindow(); return true
    }
    func showWindow() {
        mainWindow?.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminating else { return .terminateLater }
        terminating = true
        Task {
            await MonitorModel.shared.shutdown()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}

private struct WindowCapture: NSViewRepresentable {
    let captured: (NSWindow) -> Void
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { if let window = view.window { captured(window) } }
        return view
    }
    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async { if let window = nsView.window { captured(window) } }
    }
}
