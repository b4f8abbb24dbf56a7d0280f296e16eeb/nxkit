// The game can run silently when foreground audio is unavailable. An interruption
// pauses gameplay until it ends, or audio becomes available after suspension.
struct GameLifecycle {
    private(set) var foreground = false
    private var backgrounded = false
    private var interrupted = false
    private var recoveringInterruption = false

    var gameActive: Bool { foreground && !interrupted }
    var canRequestAudio: Bool { foreground && (!interrupted || recoveringInterruption) }

    mutating func setForeground(_ active: Bool) {
        foreground = active
        if active && backgrounded {
            backgrounded = false
            recoveringInterruption = interrupted
        }
    }

    mutating func enterBackground() {
        foreground = false
        backgrounded = true
        recoveringInterruption = false
    }

    mutating func interruption(began: Bool) {
        interrupted = began
        recoveringInterruption = false
    }

    mutating func audioActivated() {
        if recoveringInterruption {
            interrupted = false
            recoveringInterruption = false
        }
    }
}
