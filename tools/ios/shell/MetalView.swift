import UIKit
import QuartzCore

@MainActor
final class MetalView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    var active = false {
        didSet {
            if !active { clearTouches() }
            if overlay != nil { nx2ipa_overlay_active(active) }
        }
    }
    private var overlay: UIView?
    private var fingers = TouchState()
    private var touchSize = CGSize.zero

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .black
    }
    required init?(coder: NSCoder) { fatalError("Programmatic view only") }

    override func layoutSubviews() {
        super.layoutSubviews()
        if touchSize != bounds.size { touchSize = bounds.size; clearTouches() }
        let scale = window?.windowScene?.screen.scale ?? 1
        let width = bounds.width * scale, height = bounds.height * scale
        guard width.isFinite, height.isFinite, width > 0, height > 0,
              width <= CGFloat(UInt32.max), height <= CGFloat(UInt32.max) else { return }
        metalLayer.contentsScale = scale
        metalLayer.drawableSize = CGSize(width: width.rounded(.down), height: height.rounded(.down))
        nx2ipa_resize(UInt32(width), UInt32(height))
    }
    func installOverlay() {
        guard overlay == nil else { return }
        clearTouches()
        let controls = nx2ipa_overlay_create(bounds)
        controls.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(controls)
        overlay = controls
        nx2ipa_overlay_active(active)
    }
    func clearTouches() {
        fingers.clear()
        nx2ipa_touches(nil, 0)
        if overlay != nil { nx2ipa_overlay_reset() }
    }
    private func update(_ touches: Set<UITouch>, phase: Int) {
        guard active else { clearTouches(); return }
        let aspect = nx2ipa_picture_aspect()
        for touch in touches {
            let identity = UInt(bitPattern: ObjectIdentifier(touch))
            if phase == 2 { fingers.end(identity); continue }
            let point = touch.location(in: self)
            let normalized = normalizeTouch(x: Double(point.x - bounds.minX), y: Double(point.y - bounds.minY),
                                            width: Double(bounds.width), height: Double(bounds.height),
                                            aspect: aspect == 0 ? nil : aspect, clamp: phase == 1)
            if phase == 0 { fingers.begin(identity, point: normalized) }
            else { fingers.move(identity, point: normalized) }
        }
        let points = fingers.snapshot.map { Nx2ipaTouch(id: $0.id, x: $0.x, y: $0.y) }
        points.withUnsafeBufferPointer { nx2ipa_touches($0.baseAddress, $0.count) }
    }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { update(touches, phase: 0) }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { update(touches, phase: 1) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { update(touches, phase: 2) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { update(touches, phase: 2) }
}
