import UIKit
import Darwin

/// Two local launch logs. Redirect stderr directly to disk, not through an async
/// pipe: Rust's fatal exit(1) must not outrun a log-draining callback.
@MainActor
final class StartupDiagnostics {
    static let shared = StartupDiagnostics()
    private(set) var current: URL?
    private(set) var previous: URL?
    private var pending: URL?
    private var capture: LaunchLogCapture?
    private let retainedBytes = 256 * 1024
    private(set) var previousStartupInterrupted = false

    func install() {
        guard current == nil else { return }
        do {
            let directory = try FileManager.default.url(for: .applicationSupportDirectory,
                in: .userDomainMask, appropriateFor: nil, create: true)
                .appendingPathComponent("LaunchDiagnostics", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var excluded = directory
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try excluded.setResourceValues(values)
            let marker = directory.appendingPathComponent("startup.pending")
            let latest = directory.appendingPathComponent("launch.log")
            let last = directory.appendingPathComponent("previous-launch.log")
            previousStartupInterrupted = FileManager.default.fileExists(atPath: marker.path)
            // If the user only reviewed a failure last time, preserve that failure
            // instead of replacing it with an empty diagnostic-screen launch.
            if FileManager.default.fileExists(atPath: latest.path) {
                if FileManager.default.fileExists(atPath: last.path) {
                    try FileManager.default.removeItem(at: last)
                }
                trim(latest)
                try FileManager.default.moveItem(at: latest, to: last)
            }
            if FileManager.default.fileExists(atPath: last.path) { trim(last) }
            previous = FileManager.default.fileExists(atPath: last.path) ? last : nil
            pending = marker
            current = latest
        } catch {
            NSLog("nxkit: cannot prepare launch diagnostics: %@", error.localizedDescription)
        }
    }

    /// Called only after the recovery screen is dismissed, before native startup.
    func begin() {
        guard let current, let pending else { return }
        do {
            try Data("Native startup has not reached its ten-second checkpoint.\n".utf8)
                .write(to: pending, options: .atomic)
            let saved = current.path.withCString { nxkit_open_launch_log($0) }
            guard saved >= 0 else {
                throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            }
            let capture = LaunchLogCapture(saved: saved, path: current.path, limit: retainedBytes)
            self.capture = capture
            // Restoration must run even if native startup blocks UIKit's thread.
            DispatchQueue.global().asyncAfter(deadline: .now() + 10) { capture.finish() }
            record("launch started; \(ProcessInfo.processInfo.operatingSystemVersionString); iOS-on-Mac=\(ProcessInfo.processInfo.isiOSAppOnMac)")
        } catch {
            NSLog("nxkit: cannot capture launch stderr: %@", error.localizedDescription)
        }
    }

    func record(_ message: String) {
        // No environment dump, credentials, or game files are added to the log.
        let line = "nxkit-launch [\(Date().timeIntervalSince1970)]: \(message)\n"
        line.withCString { pointer in _ = Darwin.write(STDERR_FILENO, pointer, strlen(pointer)) }
    }

    private func trim(_ url: URL) {
        if url.path.withCString({ nxkit_trim_launch_log($0, retainedBytes) }) != 0 {
            NSLog("nxkit: could not trim launch diagnostics")
        }
    }

    func finishCapture() {
        capture?.finish()
        capture = nil
    }

    func checkpoint() {
        record("ten-second startup checkpoint reached (not a gameplay validation)")
        finishCapture()
        if let pending { try? FileManager.default.removeItem(at: pending) }
    }

    func share(_ url: URL, from controller: UIViewController, source: UIView) {
        let share = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        share.popoverPresentationController?.sourceView = source
        share.popoverPresentationController?.sourceRect = source.bounds
        controller.present(share, animated: true)
    }
}

// The lock owns the saved descriptor, allowing the deadline and returned-error
// path to race safely without double-closing a descriptor reused by the guest.
private final class LaunchLogCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var saved: Int32
    private let path: String
    private let limit: Int
    init(saved: Int32, path: String, limit: Int) {
        self.saved = saved
        self.path = path
        self.limit = limit
    }
    func finish() {
        lock.lock()
        defer { lock.unlock() }
        guard saved >= 0 else { return }
        let descriptor = saved
        saved = -1
        guard nxkit_close_launch_log(descriptor) == 0 else {
            NSLog("nxkit: could not restore stderr after startup capture")
            return
        }
        if path.withCString({ nxkit_trim_launch_log($0, limit) }) != 0 {
            NSLog("nxkit: could not trim launch diagnostics")
        }
    }
}
