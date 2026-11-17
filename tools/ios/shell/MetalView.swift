import UIKit
import QuartzCore

@MainActor
final class MetalView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    var active = false {
        didSet {
            if overlay != nil { nxkit_overlay_active(active) }
        }
    }
    private var overlay: UIView?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
    }
    required init?(coder: NSCoder) { fatalError("Programmatic view only") }

    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.windowScene?.screen.scale ?? 1
        let width = bounds.width * scale, height = bounds.height * scale
        guard width.isFinite, height.isFinite, width > 0, height > 0,
              width <= CGFloat(UInt32.max), height <= CGFloat(UInt32.max) else { return }
        metalLayer.contentsScale = scale
        metalLayer.drawableSize = CGSize(width: width.rounded(.down), height: height.rounded(.down))
        nxkit_resize(UInt32(width), UInt32(height))
    }
    func installOverlay() {
        guard overlay == nil else { return }
        let controls = nxkit_overlay_create(bounds)
        controls.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(controls)
        overlay = controls
        nxkit_overlay_active(active)
    }
}
