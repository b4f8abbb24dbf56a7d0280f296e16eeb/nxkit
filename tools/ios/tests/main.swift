// Run on macOS with swiftc tools/ios/shell/Lifecycle.swift tools/ios/tests/main.swift -o /tmp/nxkit-lifecycle-test.
var state = GameLifecycle()
precondition(!state.gameActive && !state.canRequestAudio)
state.setForeground(true)
// A newly foreground game can play silently if audio activation fails.
precondition(state.gameActive && state.canRequestAudio)

state.interruption(began: true)
precondition(!state.gameActive && !state.canRequestAudio)
state.setForeground(false)
state.setForeground(true)
// Dismissing a transient overlay does not end an audio interruption.
precondition(!state.gameActive && !state.canRequestAudio)
state.interruption(began: false)
precondition(state.gameActive && state.canRequestAudio)

state.interruption(began: true)
state.enterBackground()
state.setForeground(false) // Reconnected scene, not yet active.
precondition(!state.gameActive && !state.canRequestAudio)
state.setForeground(true)
// With no end event after suspension, keep the guest paused until activation succeeds.
precondition(!state.gameActive && state.canRequestAudio)
state.setForeground(true)
precondition(!state.gameActive && state.canRequestAudio)
state.audioActivated()
precondition(state.gameActive && state.canRequestAudio)

state.enterBackground()
state.interruption(began: true)
state.interruption(began: false)
precondition(!state.gameActive && !state.canRequestAudio)
state.setForeground(true)
precondition(state.gameActive && state.canRequestAudio)

state.interruption(began: true)
state.enterBackground()
state.setForeground(true)
state.interruption(began: true) // A fresh interruption cancels the recovery attempt.
state.audioActivated()
precondition(!state.gameActive && !state.canRequestAudio)
