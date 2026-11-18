// Nintendo button mapping, diamond and stick dead zone follow nxnative's DmGamepad.m.
// Copyright (c) 2026 seedds. MIT; see root THIRD_PARTY.md.
import UIKit

@MainActor
final class Controls: UIView {
    private struct Contact {
        let control: Int
        var point: CGPoint
    }
    private var contacts: [UITouch: Contact] = [:]
    private var fingers: [UITouch: UInt32] = [:]
    private var buttons: [CGRect] = []
    private var sticks: [CGPoint] = []
    private var dpad = CGPoint.zero
    private let radius: CGFloat = 36
    private let labels = ["A", "B", "X", "Y", "L3", "R3", "L", "R", "ZL", "ZR", "+", "−"]
    // Controls 0...11: buttons, 12/13: sticks, 14: D-pad.
    init() {
        super.init(frame: .zero)
        isMultipleTouchEnabled = true
        isOpaque = false
        backgroundColor = .clear
    }
    required init?(coder: NSCoder) { fatalError("Programmatic view") }
    override func layoutSubviews() {
        super.layoutSubviews()
        reset()
        let area = bounds.inset(by: safeAreaInsets).insetBy(dx: 16, dy: 16)
        let left = area.minX, right = area.maxX, top = area.minY, bottom = area.maxY
        let face = CGPoint(x: right - 42, y: bottom - 44)
        let centers = [
            CGPoint(x: face.x + 32, y: face.y), CGPoint(x: face.x, y: face.y + 32),
            CGPoint(x: face.x, y: face.y - 32), CGPoint(x: face.x - 32, y: face.y),
            CGPoint(x: left + 100, y: top + 20), CGPoint(x: right - 100, y: top + 20),
            CGPoint(x: left + 24, y: top + 64), CGPoint(x: right - 24, y: top + 64),
            CGPoint(x: left + 24, y: top + 20), CGPoint(x: right - 24, y: top + 20),
            CGPoint(x: area.midX + 32, y: top + 20), CGPoint(x: area.midX - 32, y: top + 20)
        ]
        buttons = centers.map { CGRect(x: $0.x - 18, y: $0.y - 18, width: 36, height: 36) }
        sticks = [CGPoint(x: left + 40, y: bottom - 50), CGPoint(x: right - 160, y: bottom - 50)]
        dpad = CGPoint(x: left + 140, y: bottom - 50)
    }
    private func offset(_ point: CGPoint, from center: CGPoint) -> CGPoint {
        let x = point.x - center.x, y = point.y - center.y
        let length = hypot(x, y)
        let dead = radius * 0.12
        guard length > dead else { return .zero }
        let magnitude = min((length - dead) / (radius - dead), 1) / length
        return CGPoint(x: x * magnitude, y: -y * magnitude)
    }
    private func publish() {
        var mask: UInt64 = 0
        var axes = [CGPoint.zero, .zero]
        for contact in contacts.values {
            switch contact.control {
            case 0..<12: mask |= UInt64(1) << contact.control
            case 12, 13: axes[contact.control - 12] = offset(contact.point, from: sticks[contact.control - 12])
            default:
                let direction = offset(contact.point, from: dpad)
                if direction.x < -0.35 { mask |= 1 << 12 }
                if direction.y > 0.35 { mask |= 1 << 13 }
                if direction.x > 0.35 { mask |= 1 << 14 }
                if direction.y < -0.35 { mask |= 1 << 15 }
            }
        }
        nxkit_virtual_pad(mask, Float(axes[0].x), Float(axes[0].y), Float(axes[1].x), Float(axes[1].y))
        let points = fingers.map { touch, id -> NxkitTouch in
            let point = touch.location(in: self)
            return NxkitTouch(id: id, x: Float(min(max(point.x / max(bounds.width, 1), 0), 1)),
                              y: Float(min(max(point.y / max(bounds.height, 1), 0), 1)))
        }
        points.withUnsafeBufferPointer { nxkit_touches($0.baseAddress, $0.count) }
        setNeedsDisplay()
    }
    func reset() {
        contacts.removeAll()
        fingers.removeAll()
        nxkit_virtual_pad(0, 0, 0, 0, 0)
        nxkit_touches(nil, 0)
        setNeedsDisplay()
    }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            let point = touch.location(in: self)
            var control = buttons.indices.first { buttons[$0].contains(point) }
            if control == nil {
                control = sticks.indices.first { hypot(point.x - sticks[$0].x, point.y - sticks[$0].y) <= radius + 8 }.map { $0 + 12 }
            }
            if control == nil && hypot(point.x - dpad.x, point.y - dpad.y) <= radius { control = 14 }
            if let control, !contacts.values.contains(where: { $0.control == control }) {
                contacts[touch] = Contact(control: control, point: point)
            } else if control == nil, let id = (UInt32(0)..<16).first(where: { !fingers.values.contains($0) }) {
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
    override func draw(_ rect: CGRect) {
        let text: [NSAttributedString.Key: Any] = [.font: UIFont.systemFont(ofSize: 14, weight: .semibold), .foregroundColor: UIColor.white]
        for (index, box) in buttons.enumerated() {
            (contacts.values.contains { $0.control == index } ? UIColor.white.withAlphaComponent(0.5) : UIColor.black.withAlphaComponent(0.35)).setFill()
            UIBezierPath(ovalIn: box).fill()
            let label = labels[index] as NSString, size = label.size(withAttributes: text)
            label.draw(at: CGPoint(x: box.midX - size.width / 2, y: box.midY - size.height / 2), withAttributes: text)
        }
        for (index, center) in sticks.enumerated() {
            UIColor.white.withAlphaComponent(0.3).setStroke()
            UIBezierPath(ovalIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)).stroke()
            let contact = contacts.values.first { $0.control == index + 12 }
            let axis = contact.map { offset($0.point, from: center) } ?? .zero
            UIColor.white.withAlphaComponent(0.4).setFill()
            UIBezierPath(ovalIn: CGRect(x: center.x + axis.x * radius - 14, y: center.y - axis.y * radius - 14, width: 28, height: 28)).fill()
        }
        UIColor.white.withAlphaComponent(0.35).setFill()
        UIBezierPath(rect: CGRect(x: dpad.x - 10, y: dpad.y - radius, width: 20, height: radius * 2)).fill()
        UIBezierPath(rect: CGRect(x: dpad.x - radius, y: dpad.y - 10, width: radius * 2, height: 20)).fill()
    }
}
