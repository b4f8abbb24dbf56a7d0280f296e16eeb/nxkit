// Runs as a native macOS command-line executable in CI, without UIKit or a game.
func point(_ x: Double, _ y: Double, _ w: Double, _ h: Double,
           _ aspect: Double? = nil, _ clamp: Bool = false) -> (Float, Float)? {
    normalizeTouch(x: x, y: y, width: w, height: h, aspect: aspect, clamp: clamp)
}
func equal(_ value: (Float, Float)?, _ x: Float, _ y: Float) -> Bool {
    value?.0 == x && value?.1 == y
}
precondition(equal(point(25, 75, 100, 100), 0.25, 0.75))
precondition(equal(point(100, 50, 200, 100, 1), 0.5, 0.5))
precondition(point(10, 50, 200, 100, 1) == nil)
precondition(equal(point(10, 50, 200, 100, 1, true), 0, 0.5))
precondition(point(50, 10, 100, 200, 1) == nil)
precondition(equal(point(50, 100, 100, 200, 1), 0.5, 0.5))
precondition(point(.nan, 0, 100, 100) == nil)
precondition(point(0, 0, 0, 100) == nil)
precondition(point(0, 0, 100, 100, 0) == nil)
precondition(point(0, 0, 100, 100, .infinity) == nil)
var state = TouchState()
state.begin(100, point: (0.2, 0.3))
state.begin(200, point: (0.4, 0.5))
state.end(100)
state.move(200, point: (0.8, 0.9))
state.begin(300, point: (0.1, 0.1))
precondition(state.snapshot == [TouchPoint(id: 0, x: 0.1, y: 0.1), TouchPoint(id: 1, x: 0.8, y: 0.9)])
state.begin(200, point: (0, 0))
precondition(state.snapshot.count == 2)
state.clear()
state.begin(1, point: nil)
precondition(state.snapshot.isEmpty)
for id in 0..<20 { state.begin(UInt(id), point: (0.5, 0.5)) }
precondition(state.snapshot.count == 16 && state.snapshot.last?.id == 15)
state.clear() // The same reset is used for rotation, inactive and scene disconnect.
state.move(1, point: (0.1, 0.2))
precondition(state.snapshot.isEmpty)
print("Swift touch identity, capacity, cancellation and picture geometry checks passed")
