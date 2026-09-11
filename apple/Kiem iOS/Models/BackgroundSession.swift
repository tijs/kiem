import BackgroundTasks
import UIKit
import os

/// Privacy-safe lifecycle logger for the iOS background pairing session. Logs
/// only event names and bounded numeric durations (deadline/elapsed seconds),
/// with dynamic numeric values marked `.public` so they're captured rather than
/// redacted by OSLog. Never logs pairing tickets, endpoint IDs, credentials,
/// peer IDs, or connection strings.
private let pairingSessionLog = Logger(subsystem: "org.tijs.kiem.ios", category: "pairing.background")

/// Which background-execution mechanism keeps the pairing mesh discoverable while
/// the scene is backgrounded. Distinguishing the two lets the UI (and the model)
/// stay honest about how much background time is really being granted.
enum BackgroundSessionKind: Equatable {
    /// A full iOS 26 `BGContinuedProcessingTask` handoff — realistically spans
    /// the whole remaining pairing window in the background.
    case continuedProcessing
    /// The legacy `UIApplication.beginBackgroundTask` fallback — only a short
    /// grace period (~seconds–30s) of background time, not the full window.
    case briefFallback
}

/// A bounded background-execution request. iOS's background-task API grants a
/// short, best-effort slice of execution time after the app leaves the
/// foreground so the app can finish an in-flight piece of work. Kiem uses it to
/// keep the pairing mesh discoverable while the user backgrounds the app to
/// paste a pairing code on another device.
///
/// On iOS 26 the preferred mechanism is a `BGContinuedProcessingTask`, which is
/// designed for exactly this: a *user-initiated* workload that may keep running
/// after the app backgrounds, with background CPU/network access by default and
/// a Live Activity so the user knows work is in progress. It is still subject
/// to expiration (changing system conditions, the user cancelling the
/// activity, or the work appearing stalled), so there is no hard guarantee of
/// time — the caller always treats `onExpire` as the end of the session and
/// tears the mesh down deterministically. When continued processing can't be
/// submitted (Simulator, not permitted, system load), we fall back to the
/// legacy `UIApplication.beginBackgroundTask` brief request.
@MainActor
protocol BackgroundTaskProviding {
    /// Begin a background session so the pairing mesh can stay discoverable
    /// while the scene is backgrounded. `durationSecs` is the pairing window's
    /// remaining time, used for progress reporting on the continued-processing
    /// path. `onExpire` runs on the main actor when the system ends the budget
    /// or interrupts the session. Returns a handle once granted, or nil when no
    /// background time is available right now (the caller then tears the mesh
    /// down rather than leave it running backgrounded unwatched).
    func begin(durationSecs: Int, onExpire: @escaping @MainActor () -> Void) -> BackgroundTaskHandle?
}

/// Handle for an active background session; call `end()` to release it early
/// (foreground return, window close, or budget expiry). Carry the granted
/// mechanism (`kind`) so the model/UI can distinguish a full continued-
/// processing handoff from a brief fallback.
struct BackgroundTaskHandle {
    /// The granted mechanism, so the model/UI can distinguish a full
    /// continued-processing handoff from a brief fallback.
    var kind: BackgroundSessionKind = .briefFallback
    /// Call `end()` to release the session early (foreground return, window
    /// close, or budget expiry). Last property so trailing-closure init works:
    /// `BackgroundTaskHandle { ... }` and `BackgroundTaskHandle(kind: .x) { ... }`.
    let end: @MainActor () -> Void
}

/// Carries a non-Sendable system value across a single isolation hop. The
/// `BGContinuedProcessingTask` the scheduler hands us is not `Sendable` (a
/// UIKit/framework class), so the nonisolated launch trampoline boxes it and the
/// `@MainActor` hop reads it. Safe because the boxed task is only ever consumed
/// on the main actor — inside `handleLive`, set up and torn down entirely on
/// main — and the box lives only for the lifetime of that hop.
final class TaskValueBox: @unchecked Sendable {
    let continued: BGContinuedProcessingTask?
    init(_ continued: BGContinuedProcessingTask?) { self.continued = continued }
}

/// The shared production provider: prefers an iOS 26 `BGContinuedProcessingTask`
/// for the user-initiated pairing session and falls back to the legacy bounded
/// `UIApplication` request when continued processing can't be submitted. A
/// singleton so the system's task-handler registration happens exactly once and
/// a single in-flight session's state (live task, progress, expiration) is
/// coherent.
@MainActor
final class DefaultBackgroundSessionProvider: BackgroundTaskProviding {
    static let shared = DefaultBackgroundSessionProvider()

    /// The continued-processing task identifier. Registered lazily the first
    /// time a pairing window is kept alive; the Info.plist whitelists the
    /// family `org.tijs.kiem.ios.continuedProcessingTask.*`.
    private static let taskIdentifier = "org.tijs.kiem.ios.continuedProcessingTask.pairing"
    /// Registration must happen only once per identifier; `BGTaskScheduler`
    /// kills the app on a second registration, so guard with a static flag that
    /// survives provider instances.
    private static var didRegister = false

    /// The live continued-processing task the system handed us, if any.
    private var liveTask: BGContinuedProcessingTask?
    /// The model's `onExpire`: what to run when the system ends the session.
    /// Nil once the session is released, so an obsolete callback is a no-op.
    private var onExpire: (@MainActor () -> Void)?
    /// Whether the current session (if any) is still active; guards the
    /// one-way completion so a late expiration or release can't double-complete,
    /// and so a fallback expiration that races an early foreground return is
    /// dropped instead of tearing down the foreground UI.
    private var isEnded = true
    /// Remaining pairing seconds, the `totalUnitCount` baseline for progress.
    private var progressTotal = 0
    private var progressTimer: Timer?
    /// When the current session began, to log a bounded elapsed duration on
    /// end/expiry. `nil` while (or once) no session is active.
    private var startDate: Date?

    private init() {}

    func begin(durationSecs: Int, onExpire: @escaping @MainActor () -> Void) -> BackgroundTaskHandle? {
        self.onExpire = onExpire
        self.isEnded = false
        progressTotal = durationSecs
        startDate = Date()
        // Preferred path: continued processing, which can realistically span the
        // whole five-minute window.
        if beginContinuedProcessing() {
            pairingSessionLog.notice("background pairing: submitted (continued-processing, duration=\(durationSecs, privacy: .public)s)")
            return BackgroundTaskHandle(kind: .continuedProcessing) { [weak self] in self?.end() }
        }
        // Revert partial continued-processing state; it is not active.
        isEnded = true
        if Self.didRegister {
            BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.taskIdentifier)
        }
        // Fallback: legacy bounded UIApplication request. A brief best-effort
        // grace period keeps discovery alive for a few extra moments where the
        // newer API is unavailable.
        pairingSessionLog.notice("background pairing: continued-processing rejected; using brief fallback")
        return beginBounded(onExpire: onExpire)
    }

    /// Submit the continued-processing request. Returns false (→ fall back to
    /// the bounded request) if the scheduler rejects it for any reason
    /// (Simulator `Unavailable`, `NotPermitted`, load, etc.).
    ///
    /// Uses `strategy = .fail` (rather than the `.queue` default): a successful
    /// submit under `.queue` only promises a queued request, not that the task
    /// has begun or that background runtime is available *now*. For a
    /// time-sensitive pairing handoff we require immediate eligibility and treat
    /// rejection as a signal to fall back (or stop safely).
    private func beginContinuedProcessing() -> Bool {
        if !ensureRegistered() { return false }
        let request = BGContinuedProcessingTaskRequest(
            identifier: Self.taskIdentifier,
            title: "Kiem pairing is active",
            subtitle: "Paste the code on your other device"
        )
        request.strategy = .fail
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // Event logged without the error's description: a mapped scheduler
            // error could carry identifiers we must never log. `begin` already
            // logs the fallback decision.
            pairingSessionLog.notice("background pairing: continued-processing submit rejected")
            return false
        }
        return true
    }

    private func ensureRegistered() -> Bool {
        guard !Self.didRegister else { return true }
        // The launch handler MUST be a nonisolated trampoline: BGTaskScheduler
        // invokes it on its own private queue, never the main actor. It is passed
        // here as a DIRECT function reference (`Self.handleLaunchedTask`), NOT an
        // inline closure literal. Building a closure literal inside this @MainActor
        // method — even `{ task in Self.handleLaunchedTask(task) }`, which captures
        // only the type — is inferred by Swift 6 as @MainActor-isolated (the SDK's
        // launchHandler parameter is imported non-@Sendable, so the closure inherits
        // the enclosing method's actor isolation and captures dynamic `self`). When
        // BGTaskScheduler calls that isolated closure off-main, the runtime's
        // isolation precheck traps with `swift_task_checkIsolatedSwift` →
        // `_dispatch_assert_queue_fail` → EXC_BREAKPOINT/SIGTRAP (the 0.4.1(6)
        // backgrounding crash). A `nonisolated` function reference carries its own
        // isolation and cannot be re-inferred from this actor scope. All provider
        // work happens inside the explicit hop in `handleLaunchedTask`/`dispatchLaunch`.
        let ok = BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.taskIdentifier,
            using: nil,
            launchHandler: Self.handleLaunchedTask
        )
        Self.didRegister = ok
        return ok
    }

    /// Nonisolated launch trampoline — the EXACT handler registered (by function
    /// reference) as `BGTaskScheduler`'s `launchHandler`. Scheduler-invoked
    /// off-main; never touches `@MainActor` provider state synchronously.
    /// Classifies the system task and carries the (non-Sendable)
    /// `BGContinuedProcessingTask` into an explicit `Task { @MainActor }` hop via
    /// a Sendable box (`dispatchLaunch`), then jumps. Completion stays exact-once
    /// through the provider's `isEnded` guard — whether the task is the pairing
    /// continued-processing task or a mis-registered task of another type.
    nonisolated static func handleLaunchedTask(_ task: BGTask) {
        launchBoxed(TaskValueBox(task as? BGContinuedProcessingTask))
    }

    /// The launch handler's actual body, extracted behind a testable seam: a
    /// `BGTask` cannot be constructed on the Simulator and BGTaskScheduler does
    /// not fire there, so this is what the unit test drives off-main to prove the
    /// registered handler path (the `Self.handleLaunchedTask` function reference)
    /// performs no synchronous `@MainActor` work and hops via `dispatchLaunch`.
    /// A `nil` box exercises the wrong/absent-task-type defensive completion.
    nonisolated static func launchBoxed(_ box: TaskValueBox) {
        dispatchLaunch {
            guard let continued = box.continued else {
                // Wrong task type for our identifier: complete defensively.
                shared.completeAndRelease(success: false)
                return
            }
            shared.handleLive(continued)
        }
    }

    /// Nonisolated hop — the single seam every OS-triggered trampoline routes
    /// through (`handleLaunchedTask`, `handleContinuedExpirationTrampoline`,
    /// `handleFallbackExpirationTrampoline`). Also the unit-test seam, because
    /// `BGTaskScheduler`/`UIApplication` callbacks are unavailable on the
    /// Simulator. Performs no `@MainActor` work synchronously: `work` is only ever
    /// run inside an explicit `Task` hop to the main actor, never on the off-main
    /// caller thread. The `@MainActor` parameter type also makes the synchronous
    /// (trapping) form a compile error rather than a runtime crash.
    nonisolated static func dispatchLaunch(_ work: @escaping @MainActor () -> Void) {
        Task { @MainActor in work() }
    }

    /// Nonisolated trampoline used (by function reference) as the continued-task
    /// `expirationHandler`. Scheduler invokes it on the completion queue
    /// (off-main); it performs no `@MainActor` work synchronously and hops through
    /// `dispatchLaunch`. Function reference — NOT a closure literal formed in a
    /// `@MainActor` method — so it cannot be re-inferred as main-actor isolated.
    nonisolated static func handleContinuedExpirationTrampoline() {
        dispatchLaunch { Self.shared.handleContinuedExpiration() }
    }

    /// Nonisolated trampoline used (by function reference) as the legacy
    /// `UIApplication.beginBackgroundTask` `expirationHandler`, which the system
    /// invokes on its expiration queue (off-main). Hops through `dispatchLaunch`.
    /// Function reference for the same isolation reason as the others.
    nonisolated static func handleFallbackExpirationTrampoline() {
        dispatchLaunch { Self.shared.handleFallbackExpiration() }
    }

    /// The system started our continued task: attach progress + expiration and
    /// start reporting so the scheduler never sees a stalled workload.
    private func handleLive(_ task: BGContinuedProcessingTask) {
        guard !isEnded else {
            // The session was already released around the time the task arrived.
            task.setTaskCompleted(success: false)
            return
        }
        liveTask = task
        task.progress.totalUnitCount = Int64(Swift.max(1, progressTotal))
        task.progress.completedUnitCount = 0
        // Nonisolated trampoline by function reference (NOT an inline closure
        // literal formed in this @MainActor method) — the scheduler invokes the
        // expiration handler on its completion queue (off-main), and a closure
        // literal built in an actor-isolated scope gets inferred @MainActor and
        // hits the same `swift_task_checkIsolatedSwift` trap as the launch
        // handler. The trampoline stays nonisolated and hops via `dispatchLaunch`.
        task.expirationHandler = Self.handleContinuedExpirationTrampoline
        startProgressTimer()
        reportProgress()
        pairingSessionLog.notice("background pairing: launched (continued-processing)")
    }

    private func startProgressTimer() {
        progressTimer?.invalidate()
        progressTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.reportProgress() }
        }
    }

    private func reportProgress() {
        guard let task = liveTask else { return }
        if task.progress.completedUnitCount >= task.progress.totalUnitCount {
            // The pairing window fully elapsed — nothing left to keep alive.
            completeAndRelease(success: true)
            return
        }
        task.progress.completedUnitCount += 1
    }

    /// System expiration of the continued task. Detaches from the OS task and
    /// marks the session ended FIRST (so the model's cleanup, which calls
    /// `handle.end()` / `completeAndRelease`, cannot double-complete as success),
    /// then runs cleanup (resolve approval, close/invalidate pairing, stop the
    /// mesh) and only THEN reports the task failure exactly once.
    private func handleContinuedExpiration() {
        guard !isEnded else { return }
        let expire = onExpire
        let elapsed = consumeElapsed()
        pairingSessionLog.notice("background pairing: expired (continued-processing, elapsed=\(elapsed ?? 0, privacy: .public)s)")
        // One-way end, before any cleanup can re-enter through the handle.
        isEnded = true
        onExpire = nil
        progressTimer?.invalidate()
        progressTimer = nil
        let task = liveTask
        liveTask = nil
        if Self.didRegister {
            BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.taskIdentifier)
        }
        // Cleanup before reporting failure, per the reliability guidance.
        expire?()
        task?.setTaskCompleted(success: false)
        task?.expirationHandler = nil
    }

    /// One-way end of a session: complete the live task (if any), cancel any
    /// pending request, stop progress. Safe to call multiple times.
    private func completeAndRelease(success: Bool) {
        guard !isEnded else { return }
        isEnded = true
        let elapsed = consumeElapsed()
        pairingSessionLog.notice("background pairing: ended (success=\(success, privacy: .public), elapsed=\(elapsed ?? 0, privacy: .public)s)")
        progressTimer?.invalidate()
        progressTimer = nil
        let task = liveTask
        liveTask = nil
        if let task {
            task.setTaskCompleted(success: success)
            task.expirationHandler = nil
        }
        BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.taskIdentifier)
        onExpire = nil
    }

    /// Release the session early (foreground return, window close, deadline).
    func end() {
        completeAndRelease(success: true)
    }

    private func beginBounded(onExpire: @escaping @MainActor () -> Void) -> BackgroundTaskHandle? {
        self.onExpire = onExpire
        isEnded = false
        let app = UIApplication.shared
        // The expiration handler is a nonisolated trampoline by function reference
        // (NOT an inline closure literal formed in this @MainActor method): the
        // system runs it on UIApplication's expiration queue (off-main), and a
        // closure literal built in an actor-isolated scope is inferred @MainActor
        // and hits the same isolation trap as the launch handler. The trampoline
        // stays nonisolated and hops to main, where the exact-once handler cleans
        // up. The session may already have ended early (foreground return / window
        // close), in which case it is an obsolete callback and must not tear down
        // the foreground UI.
        let identifier = app.beginBackgroundTask(
            withName: "org.kiem.pairing.backgroundsession",
            expirationHandler: Self.handleFallbackExpirationTrampoline
        )
        guard identifier != .invalid else {
            isEnded = true
            return nil
        }
        return BackgroundTaskHandle(kind: .briefFallback) { [weak self] in
            guard let self else { return }
            // One-way release: mark the session ended and drop the model's
            // callback so the beginBackgroundTask completion closure — which the
            // system may fire later — is a stale no-op instead of tearing down a
            // pairing window that has returned to the foreground.
            self.isEnded = true
            self.onExpire = nil
            app.endBackgroundTask(identifier)
        }
    }

    /// Legacy fallback budget expired. If the session was already released
    /// early (foreground return / window close), the handle's `end()` already
    /// ended the OS task and nulled `onExpire` — run nothing.
    private func handleFallbackExpiration() {
        guard !isEnded else { return }
        let expire = onExpire
        let elapsed = consumeElapsed()
        pairingSessionLog.notice("background pairing: expired (brief fallback, elapsed=\(elapsed ?? 0, privacy: .public)s)")
        isEnded = true
        onExpire = nil
        expire?()
    }

    /// Read and clear the session start time, returning the bounded elapsed
    /// seconds (nil if no session was active) for a log line. Only ever logged
    /// as a whole-number duration — never an identifier.
    private func consumeElapsed() -> Int? {
        defer { startDate = nil }
        return startDate.map { max(0, Int(Date().timeIntervalSince($0))) }
    }
}