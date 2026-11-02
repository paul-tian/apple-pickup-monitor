import Foundation
import AppKit

enum LifecycleFailure: Error { case failed(String) }

@MainActor
func waitFor(_ label: String, timeout: TimeInterval = 12, _ condition: @escaping () -> Bool) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return }
        try await Task.sleep(nanoseconds: 30_000_000)
    }
    throw LifecycleFailure.failed(label)
}

@main
struct EngineLifecycleTests {
    @MainActor static func testManualRefresh(_ model: MonitorModel, root: URL) async throws {
        func require(_ condition: Bool, _ message: String) throws {
            if !condition { throw LifecycleFailure.failed(message) }
        }
        func requests(_ path: String) throws -> [[String: Any]] {
            let log = root.appendingPathComponent("monitor-requests.jsonl")
            guard FileManager.default.fileExists(atPath: log.path) else { return [] }
            return try String(contentsOf: log, encoding: .utf8).split(separator: "\n").map {
                try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String: Any]
            }.filter { $0["path"] as? String == path }
        }
        func mark(_ name: String) throws { try Data().write(to: root.appendingPathComponent(name)) }

        try require(model.canRefreshNow, "paused monitor with targets cannot refresh")
        try mark("empty-targets")
        await model.refresh()
        try require(!model.canRefreshNow, "empty monitoring list enabled refresh")
        try FileManager.default.removeItem(at: root.appendingPathComponent("empty-targets"))
        await model.refresh()

        let hold = root.appendingPathComponent("hold-refresh-reply")
        try Data().write(to: hold)
        let pending = Task { await model.refreshNow() }
        try await waitFor("manual refresh did not become busy") { model.isBusy }
        try require(!model.canRefreshNow, "busy model enabled another refresh")
        let duplicate = await model.refreshNow()
        try require(!duplicate, "manual refresh accepted a concurrent duplicate")
        try FileManager.default.removeItem(at: hold)
        let accepted = await pending.value
        try require(accepted && model.isRefreshingStock && model.isCheckingStock, "manual refresh was not reflected in state")
        try require(!model.state.running, "one-time refresh enabled automatic monitoring")
        try require(!model.canRefreshNow && model.state.progress != nil, "active one-time refresh enabled another request or lost progress")
        let activeDuplicate = await model.refreshNow()
        try require(!activeDuplicate, "active check accepted a duplicate")
        let first = try requests("/api/monitor/refresh")
        try require(first.count == 1 && first[0]["method"] as? String == "POST", "refresh did not send exactly one POST")
        try require((first[0]["body"] as? [String: Any])?.isEmpty == true, "refresh request body changed")
        try require(try requests("/api/monitor").isEmpty, "refresh toggled monitoring instead of requesting a check")

        try mark("complete-refresh")
        await model.refresh()
        try require(!model.isRefreshingStock && !model.state.running, "one-time completion did not return to paused")
        try require(!model.canRefreshNow, "manual cooldown was ignored")
        let cooling = await model.refreshNow()
        try require(!cooling && (try requests("/api/monitor/refresh")).count == 1, "cooldown sent another request")
        try mark("expire-refresh-cooldown")
        await model.refresh()
        try require(model.canRefreshNow, "expired cooldown did not permit refresh")

        let saved = await model.saveSettings(intervalSeconds: 30, pushoverEnabled: true, barkEnabled: true,
                                            pushoverToken: "", pushoverUser: "", barkURL: "")
        try require(saved && model.state.intervalSeconds == 30, "30-second interval was not saved")
        await model.setRunning(true)
        try require(model.state.running && model.canRefreshNow, "idle automatic monitor could not refresh")
        try mark("automatic-cycle")
        await model.refresh()
        try require(!model.canRefreshNow, "active automatic cycle allowed a duplicate refresh")
        try FileManager.default.removeItem(at: root.appendingPathComponent("automatic-cycle"))
        await model.refresh()
        let runningRefresh = await model.refreshNow()
        try require(runningRefresh && model.state.running && model.isRefreshingStock, "refresh changed automatic monitoring intent")
        try mark("complete-refresh")
        await model.refresh()
        try require(model.state.running && !model.isRefreshingStock && model.state.intervalSeconds == 30,
                    "refresh completion changed automatic monitoring or its saved interval")
        await model.setRunning(false)
        try mark("expire-refresh-cooldown")
        await model.refresh()

        let togglesBeforeFailure = try requests("/api/monitor").count
        try mark("reject-refresh")
        let rejected = await model.refreshNow()
        try require(!rejected && model.errorMessage == "Manual refresh rejected.", "refresh failure was not shown")
        try require(!model.state.running && !model.isRefreshingStock && !model.isBusy,
                    "failed refresh left the app running or busy")
        try require(try requests("/api/monitor").count == togglesBeforeFailure, "failed refresh enabled monitoring")
        try require(try requests("/api/monitor/refresh").count == 3, "refresh failure was replayed")
        try FileManager.default.removeItem(at: root.appendingPathComponent("reject-refresh"))
    }

    @MainActor static func testNotificationDrafts(_ model: MonitorModel, root: URL) async throws {
        func require(_ condition: Bool, _ message: String) throws {
            if !condition { throw LifecycleFailure.failed(message) }
        }
        func requests() throws -> [[String: Any]] {
            let log = root.appendingPathComponent("notification-requests.jsonl")
            guard FileManager.default.fileExists(atPath: log.path) else { return [] }
            return try String(contentsOf: log, encoding: .utf8).split(separator: "\n").map {
                try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String: Any]
            }
        }
        func lastBody() throws -> [String: String] {
            try requests().last?["body"] as? [String: String] ?? [:]
        }

        try require(!model.canTestNotification(channel: "pushover"), "empty Pushover credentials enabled the test")
        try require(!model.canTestNotification(channel: "pushover", pushoverToken: "draft-token", pushoverUser: " \n"), "incomplete draft enabled the test")
        try require(!model.canTestNotification(channel: "bark", barkURL: " \n"), "blank Bark URL enabled the test")
        try require(model.canTestNotification(channel: "pushover", pushoverToken: "draft-token", pushoverUser: "draft-user"), "unsaved Pushover credentials disabled the test")
        try require(model.canTestNotification(channel: "bark", barkURL: "https://example.invalid/draft-device"), "unsaved Bark URL disabled the test")

        let heldTest = root.appendingPathComponent("hold-test")
        try Data().write(to: heldTest)
        let pending = Task { await model.testNotification(channel: "pushover", pushoverToken: " draft-token\n", pushoverUser: " draft-user ", barkURL: "https://example.invalid/unrelated") }
        try await waitFor("notification test did not become busy") { model.isBusy }
        try require(!model.canTestNotification(channel: "bark", barkURL: "https://example.invalid/device"), "busy model enabled another test")
        let duplicate = await model.testNotification(channel: "bark", barkURL: "https://example.invalid/device")
        try require(!duplicate, "busy model sent a duplicate notification")
        try FileManager.default.removeItem(at: heldTest)
        let sentDraft = await pending.value
        try require(sentDraft, "Pushover draft test failed")
        try require(try lastBody() == ["channel": "pushover", "pushoverToken": "draft-token", "pushoverUser": "draft-user"], "Pushover test lost draft credentials or included another provider")

        let sentBark = await model.testNotification(channel: "bark", pushoverToken: "unrelated-token", pushoverUser: "unrelated-user", barkURL: " https://example.invalid/draft-device\n")
        try require(sentBark, "Bark draft test failed")
        try require(try lastBody() == ["channel": "bark", "barkUrl": "https://example.invalid/draft-device"], "Bark test lost its draft URL or included another provider")
        let beforeSave = try requests()
        try require(beforeSave.count == 2 && beforeSave.allSatisfy { $0["path"] as? String == "/api/notifications/test" }, "testing credentials sent a save request")
        try require(!model.state.settings.pushoverConfigured && !model.state.settings.barkConfigured, "testing marked unsaved credentials as saved")

        let saved = await model.saveSettings(intervalSeconds: 60, pushoverEnabled: true, barkEnabled: true,
                                            pushoverToken: "saved-token", pushoverUser: "saved-user", barkURL: "https://example.invalid/saved-device")
        try require(saved, "explicit Save settings failed")
        let storedCredentials = try Data(contentsOf: root.appendingPathComponent("saved-credentials.json"))
        try require(model.canTestNotification(channel: "pushover") && model.canTestNotification(channel: "bark"), "saved credentials could not be tested with blank fields")
        let sentSaved = await model.testNotification(channel: "pushover", pushoverToken: " \n", pushoverUser: "\t")
        try require(sentSaved && (try lastBody()) == ["channel": "pushover"], "blank fields did not request saved credentials")

        try Data().write(to: root.appendingPathComponent("reject-test"))
        let rejected = await model.testNotification(channel: "pushover", pushoverToken: " replacement-token ")
        try require(!rejected && model.errorMessage == "Test rejected by fake provider.", "provider failure was not surfaced")
        try require(try lastBody() == ["channel": "pushover", "pushoverToken": "replacement-token"], "partial draft did not preserve blank-field fallback")
        try require(model.state.settings.pushoverConfigured && model.state.settings.barkConfigured, "failed draft test changed saved credential status")
        try require(try Data(contentsOf: root.appendingPathComponent("saved-credentials.json")) == storedCredentials, "failed draft test replaced saved credentials")
        let saves = try requests().filter { ["/api/credentials", "/api/settings"].contains($0["path"] as? String ?? "") }
        try require(saves.count == 2, "test flow persisted credentials or settings without explicit Save")
        try require(!model.isBusy && model.successMessage == nil, "failed test left stale success or busy state")
    }

    @MainActor static func main() async {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("pickup-lifecycle-\(UUID().uuidString)")
        let helper = root.appendingPathComponent("engine.py")
        var model: MonitorModel?
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            let script = #"""
#!/usr/bin/env python3
import sys,json,pathlib,os,time
root=pathlib.Path(sys.argv[sys.argv.index('--data-dir')+1])
if (root/'instance-in-use').exists():
 sys.stderr.write('fake-secret-diagnostic\n')
 sys.stdout.write(json.dumps({'id':0,'status':409,'body':{'code':'instance_in_use'}})+'\n');sys.stdout.flush()
 sys.exit(1)
starts=root/'starts'
generation=int(starts.read_text())+1 if starts.exists() else 1
starts.write_text(str(generation))
running=False
refreshing=False
manual_until=0
language='en'
credentials={}
preferences={'pushoverEnabled':False,'barkEnabled':False}
allowed=time.time()+0.3 if generation>1 else 0
target={'key':'en_AU|RTEST|PART','target':{'locale':'en_AU','storeNumber':'RTEST','storeTitle':'Test store','partNumber':'PART','productName':'Test phone'},'availability':{'kind':'unknown','reason':'not_yet_checked'},'lastCheckedMs':None,'availabilityDetail':'Pending','checkPhase':None,'consecutiveFailures':0}
try:
 for line in sys.stdin:
  q=json.loads(line);path=q['path'];body=q.get('body',{});status=200
  if path in ['/api/credentials','/api/settings','/api/notifications/test']:
   with (root/'notification-requests.jsonl').open('a') as log:log.write(json.dumps(q)+'\n')
  if path in ['/api/monitor','/api/monitor/refresh']:
   with (root/'monitor-requests.jsonl').open('a') as log:log.write(json.dumps(q)+'\n')
  if path=='/api/state':
   if (root/'crash').exists():
    (root/'crash').unlink();os._exit(17)
   if (root/'complete-refresh').exists():refreshing=False;(root/'complete-refresh').unlink()
   if (root/'expire-refresh-cooldown').exists():manual_until=0;(root/'expire-refresh-cooldown').unlink()
   settings={**preferences,'pushoverConfigured':bool(credentials.get('pushoverToken') and credentials.get('pushoverUser')),'barkConfigured':bool(credentials.get('barkUrl'))}
   progress={'completedStores':0,'totalStores':1,'activeLocation':'Test store','startedAt':int(time.time()*1000),'phase':'checking','waitUntil':None} if refreshing or (root/'automatic-cycle').exists() else None
   answer={'version':'0.4.0','running':running,'intervalSeconds':preferences.get('intervalSeconds',30),'nextCheckAt':int((time.time()+60)*1000) if running and not progress else None,'paced':False,'healthy':None,'notice':None,'catalogRefreshing':False,'targets':[] if (root/'empty-targets').exists() else [target],'notifications':[],'settings':settings,'progress':progress,'nextAllowedStartAt':int(allowed*1000),'notificationLanguage':language,'refreshingStock':refreshing,'nextManualCheckAt':int(manual_until*1000)}
  elif path=='/api/regions':answer={'regions':[{'locale':'en_AU','title':'Australia','baseUrl':'https://www.apple.com/au'}]}
  elif path.startswith('/api/catalog'):answer={'locale':'en_AU','stores':[],'products':[]}
  elif path=='/api/monitor':
   if body['running'] and time.time()<allowed:status=429;answer={'error':'Cooling down'}
   else:
    running=body['running'];answer={'ok':True}
    if not running:refreshing=False
  elif path=='/api/monitor/refresh':
   while (root/'hold-refresh-reply').exists():time.sleep(0.01)
   if (root/'reject-refresh').exists():status=400;answer={'error':'Manual refresh rejected.'}
   else:refreshing=True;manual_until=time.time()+30;answer={'ok':True}
  elif path=='/api/language':language=body['language'];answer={'ok':True}
  elif path=='/api/credentials':
   credentials.update(body);(root/'saved-credentials.json').write_text(json.dumps(credentials));answer={'ok':True}
  elif path=='/api/settings':preferences.update(body);answer={'ok':True}
  elif path=='/api/notifications/test':
   while (root/'hold-test').exists():time.sleep(0.01)
   if (root/'reject-test').exists():status=400;answer={'error':'Test rejected by fake provider.'}
   else:answer={'ok':True}
  else:answer={'ok':True}
  sys.stdout.write(json.dumps({'id':q['id'],'status':status,'body':answer})+'\n');sys.stdout.flush()
finally:
 (root/('stopped-'+str(generation))).write_text('EOF')
"""#
            try script.write(to: helper, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: helper.path)
            let instance = MonitorModel(dataDirectory: root, helperURL: helper)
            model = instance
            let conflict = root.appendingPathComponent("instance-in-use")
            try Data().write(to: conflict)
            guard !instance.canTestNotification(channel: "pushover", pushoverToken: "draft-token", pushoverUser: "draft-user") else {
                throw LifecycleFailure.failed("notification test enabled before the engine was ready")
            }
            await instance.start()
            let conflictMessage = "Another copy of Apple Pickup Monitor is using this monitoring data. Quit that copy to continue."
            try await waitFor("startup conflict did not preserve its explanation") {
                !instance.isReady && instance.errorMessage == conflictMessage
            }
            try await Task.sleep(nanoseconds: 100_000_000)
            guard instance.errorMessage == conflictMessage else {
                throw LifecycleFailure.failed("generic recovery message overwrote the startup conflict")
            }
            try FileManager.default.removeItem(at: conflict)
            try await waitFor("engine did not initialize") { instance.isReady }
            guard instance.errorMessage == nil else { throw LifecycleFailure.failed("successful recovery retained the startup conflict") }
            guard !instance.state.running else { throw LifecycleFailure.failed("fresh launch started monitoring unexpectedly") }
            try await testNotificationDrafts(instance, root: root)
            try await testManualRefresh(instance, root: root)
            await instance.setRunning(true)
            try await waitFor("monitoring did not start") { instance.state.running }
            try Data().write(to: root.appendingPathComponent("crash"))
            try await waitFor("engine did not recover and resume") {
                let count = (try? String(contentsOf: root.appendingPathComponent("starts"), encoding: .utf8)) ?? "0"
                return Int(count) ?? 0 >= 2 && instance.isReady && instance.state.running
            }
            await instance.setRunning(false)
            try await waitFor("pause did not take effect") { !instance.state.running }
            try Data().write(to: root.appendingPathComponent("crash"))
            try await waitFor("paused engine did not recover") {
                let count = (try? String(contentsOf: root.appendingPathComponent("starts"), encoding: .utf8)) ?? "0"
                return Int(count) ?? 0 >= 3 && instance.isReady
            }
            guard !instance.state.running else { throw LifecycleFailure.failed("recovery ignored the user's pause") }
            await instance.shutdown()
            try await waitFor("quit did not close the parent pipe", timeout: 3) {
                FileManager.default.fileExists(atPath: root.appendingPathComponent("stopped-3").path)
            }
            guard !instance.isReady else { throw LifecycleFailure.failed("quit left the app ready") }
            try? FileManager.default.removeItem(at: root)
            print("Engine lifecycle tests passed: manual refresh and 30-second settings, startup conflict recovery, notification drafts, automatic recovery, pause preservation and EOF shutdown.")
        } catch {
            await model?.shutdown()
            try? FileManager.default.removeItem(at: root)
            fputs("Engine lifecycle test failed: \(error)\n", stderr)
            exit(1)
        }
    }
}
