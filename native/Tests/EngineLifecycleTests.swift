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
starts=root/'starts'
generation=int(starts.read_text())+1 if starts.exists() else 1
starts.write_text(str(generation))
running=False
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
  if path=='/api/state':
   if (root/'crash').exists():
    (root/'crash').unlink();os._exit(17)
   settings={**preferences,'pushoverConfigured':bool(credentials.get('pushoverToken') and credentials.get('pushoverUser')),'barkConfigured':bool(credentials.get('barkUrl'))}
   answer={'version':'0.4.0','running':running,'intervalSeconds':60,'nextCheckAt':None,'paced':False,'healthy':None,'notice':None,'catalogRefreshing':False,'targets':[target],'notifications':[],'settings':settings,'progress':None,'nextAllowedStartAt':int(allowed*1000),'notificationLanguage':language}
  elif path=='/api/regions':answer={'regions':[{'locale':'en_AU','title':'Australia','baseUrl':'https://www.apple.com/au'}]}
  elif path.startswith('/api/catalog'):answer={'locale':'en_AU','stores':[],'products':[]}
  elif path=='/api/monitor':
   if body['running'] and time.time()<allowed:status=429;answer={'error':'Cooling down'}
   else:running=body['running'];answer={'ok':True}
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
            guard !instance.canTestNotification(channel: "pushover", pushoverToken: "draft-token", pushoverUser: "draft-user") else {
                throw LifecycleFailure.failed("notification test enabled before the engine was ready")
            }
            await instance.start()
            try await waitFor("engine did not initialize") { instance.isReady }
            guard !instance.state.running else { throw LifecycleFailure.failed("fresh launch started monitoring unexpectedly") }
            try await testNotificationDrafts(instance, root: root)
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
            print("Engine lifecycle tests passed: notification drafts and explicit saves, startup, automatic recovery, resume, pause preservation and EOF shutdown.")
        } catch {
            await model?.shutdown()
            try? FileManager.default.removeItem(at: root)
            fputs("Engine lifecycle test failed: \(error)\n", stderr)
            exit(1)
        }
    }
}
