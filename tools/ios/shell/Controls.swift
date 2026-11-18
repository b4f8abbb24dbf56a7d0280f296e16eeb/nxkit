// Physical button positions: Nintendo's public Pro Controller diagram (support article 27538).
import UIKit

@MainActor
final class Controls: UIView {
    private enum Kind {
        case button(Int, String), stick(Int)
    }
    private struct Target {
        let kind: Kind
        let center: CGPoint
        let radius: CGFloat
        func contains(_ point: CGPoint) -> Bool {
            hypot(point.x - center.x, point.y - center.y) <= radius
        }
        func axis(at point: CGPoint) -> CGPoint {
            let x = (point.x - center.x) / radius, y = (center.y - point.y) / radius
            let length = hypot(x, y)
            guard length > 0.1 else { return .zero }
            let gain = min((length - 0.1) / 0.9, 1) / length
            return CGPoint(x: x * gain, y: y * gain)
        }
    }
    private struct Contact {
        let target: Int
        var point: CGPoint
    }
    private final class Surface: UIView {
        let label: UILabel?
        let knob: UIView?

        init(title: String?, radius: CGFloat, font: UIFont) {
            label = title.map { title in
                let label = UILabel()
                label.text = title
                label.font = font
                label.textAlignment = .center
                label.textColor = UIColor.white.withAlphaComponent(0.7)
                label.highlightedTextColor = .white
                label.isAccessibilityElement = false
                return label
            }
            knob = title == nil ? UIView(frame: CGRect(x: radius * 0.6, y: radius * 0.6,
                                                       width: radius * 0.8, height: radius * 0.8)) : nil
            super.init(frame: .zero)
            isOpaque = false
            backgroundColor = .clear
            if let label { addSubview(label) }
            if let knob {
                knob.isOpaque = false
                knob.backgroundColor = .clear
                addSubview(knob)
            }
            setHeld(false)
        }
        required init?(coder: NSCoder) { fatalError("Programmatic view") }

        func setHeld(_ held: Bool) {
            label?.isHighlighted = held
            // Only the outlines and legend brighten. The game stays visible through
            // every control background, including while a button or stick is held.
            layer.borderColor = UIColor.white.withAlphaComponent(held ? 1 : 0.35).cgColor
            layer.borderWidth = held ? 2 : 1
            knob?.layer.borderColor = UIColor.white.withAlphaComponent(held ? 1 : 0.55).cgColor
            knob?.layer.borderWidth = held ? 2 : 1.5
        }
        func moveKnob(to point: CGPoint) { knob?.center = point }
        override func layoutSubviews() {
            super.layoutSubviews()
            layer.cornerRadius = min(bounds.width, bounds.height) / 2
            label?.frame = bounds
            if let knob { knob.layer.cornerRadius = knob.bounds.width / 2 }
        }
    }
    private var targets: [Target] = []
    private var surfaces: [Surface] = []
    private var heldSurfaces: Set<Int> = []
    private var contacts: [UITouch: Contact] = [:]
    private var fingers: [UITouch: UInt32] = [:]
    private var layoutArea = CGRect.null

    init() {
        super.init(frame: .zero)
        isMultipleTouchEnabled = true
        isOpaque = false
        backgroundColor = .clear
    }
    required init?(coder: NSCoder) { fatalError("Programmatic view") }

    override func layoutSubviews() {
        super.layoutSubviews()
        let area = bounds.inset(by: safeAreaInsets).insetBy(dx: 8, dy: 8)
        guard area != layoutArea else { return }
        reset()
        layoutArea = area
        let scale = max(0, min(1, area.width / 400, area.height / 280))
        surfaces.forEach { $0.removeFromSuperview() }
        surfaces.removeAll()
        targets.removeAll()
        guard scale > 0 else { return }
        let font = UIFont.systemFont(ofSize: 14 * scale, weight: .semibold)
        func add(_ kind: Kind, _ x: CGFloat, _ y: CGFloat, _ radius: CGFloat = 22) {
            let radius = radius * scale
            targets.append(Target(kind: kind, center: CGPoint(x: x, y: y), radius: radius))
            let title: String?
            if case let .button(_, text) = kind { title = text } else { title = nil }
            let surface = Surface(title: title, radius: radius, font: font)
            surface.frame = CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2)
            // Controls owns every touch, including simultaneous button, stick and game-screen touches.
            surface.isUserInteractionEnabled = false
            surface.isAccessibilityElement = false
            addSubview(surface)
            surfaces.append(surface)
        }
        let l = area.minX, r = area.maxX, t = area.minY, b = area.maxY, s = scale
        // Edge-anchored clusters leave the middle of the game unobstructed.
        add(.button(8, "ZL"), l + 28*s, t + 22*s)
        add(.button(4, "L3"), l + 100*s, t + 22*s)
        add(.button(11, "−"), (l + r)/2 - 28*s, t + 22*s)
        add(.button(10, "+"), (l + r)/2 + 28*s, t + 22*s)
        add(.button(5, "R3"), r - 100*s, t + 22*s)
        add(.button(9, "ZR"), r - 28*s, t + 22*s)
        add(.button(6, "L"), l + 28*s, t + 74*s)
        add(.button(7, "R"), r - 28*s, t + 74*s)
        add(.stick(0), l + 48*s, b - 56*s, 36)
        add(.button(12, "←"), l + 118*s, b - 56*s)
        add(.button(13, "↑"), l + 150*s, b - 88*s)
        add(.button(14, "→"), l + 182*s, b - 56*s)
        add(.button(15, "↓"), l + 150*s, b - 24*s)
        let face = CGPoint(x: r - 60*s, y: b - 56*s)
        add(.button(0, "A"), face.x + 32*s, face.y)
        add(.button(1, "B"), face.x, face.y + 32*s)
        add(.button(2, "X"), face.x, face.y - 32*s)
        add(.button(3, "Y"), face.x - 32*s, face.y)
        add(.stick(1), r - 158*s, b - 56*s, 36)
    }

    private func publish() {
        var mask: UInt64 = 0
        var axes = [CGPoint.zero, .zero]
        for contact in contacts.values {
            let target = targets[contact.target]
            switch target.kind {
            case let .button(bit, _): mask |= UInt64(1) << bit
            case let .stick(index): axes[index] = target.axis(at: contact.point)
            }
        }
        nxkit_virtual_pad(mask, Float(axes[0].x), Float(axes[0].y), Float(axes[1].x), Float(axes[1].y))
        let points = fingers.sorted { $0.value < $1.value }.map { touch, id -> NxkitTouch in
            let p = touch.location(in: self)
            return NxkitTouch(id: id, x: Float(min(max(p.x / max(bounds.width, 1), 0), 1)),
                              y: Float(min(max(p.y / max(bounds.height, 1), 0), 1)))
        }
        points.withUnsafeBufferPointer { nxkit_touches($0.baseAddress, $0.count) }
        updateAppearance()
    }
    func reset() {
        contacts.removeAll()
        fingers.removeAll()
        nxkit_virtual_pad(0, 0, 0, 0, 0)
        nxkit_touches(nil, 0)
        updateAppearance()
    }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            let point = touch.location(in: self)
            if let index = targets.firstIndex(where: { $0.contains(point) }) {
                // Buttons stay down until every owner lifts. Analog controls have one owner.
                let occupied = contacts.values.contains { $0.target == index }
                let accepts: Bool
                if case .button = targets[index].kind { accepts = true } else { accepts = !occupied }
                if accepts {
                    contacts[touch] = Contact(target: index, point: point)
                }
            } else if let id = (UInt32(0)..<16).first(where: { !fingers.values.contains($0) }) {
                fingers[touch] = id
            }
        }
        publish()
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches where contacts[touch] != nil { contacts[touch]?.point = touch.location(in: self) }
        publish()
    }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            contacts.removeValue(forKey: touch)
            fingers.removeValue(forKey: touch)
        }
        publish()
    }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { touchesEnded(touches, with: event) }

    private func updateAppearance() {
        for (index, target) in targets.enumerated() {
            let contact = contacts.values.first { $0.target == index }
            let held = contact != nil
            if held != heldSurfaces.contains(index) {
                if held { heldSurfaces.insert(index) } else { heldSurfaces.remove(index) }
                let surface = surfaces[index]
                surface.setHeld(held)
                // The overlay owns touches, so child controls never receive UIKit touch tracking.
                // Drive feedback from the same owners as input, including multi-finger holds.
                let transform = held ? CGAffineTransform(scaleX: 1.08, y: 1.08) : .identity
                if UIAccessibility.isReduceMotionEnabled {
                    surface.transform = transform
                } else {
                    UIView.animate(withDuration: held ? 0.14 : 0.22, delay: 0,
                                   usingSpringWithDamping: 0.72, initialSpringVelocity: 0,
                                   options: [.allowUserInteraction, .beginFromCurrentState]) {
                        surface.transform = transform
                    }
                }
            }
            switch target.kind {
            case .button:
                break
            case .stick:
                let axis = contact.map { target.axis(at: $0.point) } ?? .zero
                let r = target.radius
                surfaces[index].moveKnob(to:
                    CGPoint(x: r + axis.x * r * 0.6, y: r - axis.y * r * 0.6))
            }
        }
    }
}
