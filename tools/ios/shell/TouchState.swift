// UIKit-independent touch identity and picture-coordinate mapping.
struct TouchPoint: Equatable {
    let id: UInt32
    var x: Float
    var y: Float
}
struct TouchState {
    private struct Finger { let identity: UInt; var point: TouchPoint }
    private var fingers: [Finger?] = Array(repeating: nil, count: 16)
    var snapshot: [TouchPoint] { fingers.compactMap { $0?.point } }
    mutating func begin(_ identity: UInt, point: (Float, Float)?) {
        guard !fingers.contains(where: { $0?.identity == identity }),
              let point, let slot = fingers.firstIndex(where: { $0 == nil }) else { return }
        fingers[slot] = Finger(identity: identity, point: TouchPoint(id: UInt32(slot), x: point.0, y: point.1))
    }
    mutating func move(_ identity: UInt, point: (Float, Float)?) {
        guard let point, let slot = fingers.firstIndex(where: { $0?.identity == identity }) else { return }
        fingers[slot]?.point.x = point.0
        fingers[slot]?.point.y = point.1
    }
    mutating func end(_ identity: UInt) {
        if let slot = fingers.firstIndex(where: { $0?.identity == identity }) { fingers[slot] = nil }
    }
    mutating func clear() { fingers = Array(repeating: nil, count: 16) }
}

// Nil aspect means the real presenter fills the drawable. Never assume 16:9.
// Reject new contacts in letterbox bars; clamp existing drags to picture edges.
func normalizeTouch(x: Double, y: Double, width: Double, height: Double,
                    aspect: Double?, clamp: Bool) -> (Float, Float)? {
    guard [x, y, width, height].allSatisfy({ $0.isFinite }), width > 0, height > 0 else { return nil }
    var w = width, h = height
    if let aspect {
        guard aspect.isFinite, aspect > 0 else { return nil }
        if width / height > aspect { w = height * aspect } else { h = width / aspect }
    }
    let px = (x - (width - w) * 0.5) / w, py = (y - (height - h) * 0.5) / h
    guard px.isFinite, py.isFinite else { return nil }
    if !clamp && (!(0...1).contains(px) || !(0...1).contains(py)) { return nil }
    return (Float(min(max(px, 0), 1)), Float(min(max(py, 0), 1)))
}
