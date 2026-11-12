// Layout picker derived directly from nxnative GameMenu.swift at
// ab41d6a177819559c76d8e4381791550ababa097. MIT; see root THIRD_PARTY.md.
// The standalone feedback and process-exit actions are nxkit integration code.
import UIKit

@MainActor
@_cdecl("nx2ipa_game_menu")
public func nx2ipaGameMenu() -> UnsafeMutableRawPointer {
    let menu = UIMenu(children: [UIDeferredMenuElement.uncached { completion in
        completion(makeGameActions())
    }])
    return Unmanaged.passRetained(menu).toOpaque()
}

@MainActor
private func makeGameActions() -> [UIMenuElement] {
    let names = ["2D", "3D", "Tap"]
    let layouts = (0..<3).filter { nx2ipa_pad_layout_offered(Int32($0)) }.map { layout in
        UIAction(title: names[layout], state: nx2ipa_pad_layout() == Int32(layout) ? .on : .off) { _ in
            nx2ipa_pad_layout_set(Int32(layout))
        }
    }
    let feedback = UIAction(title: "Touch feedback", state: nx2ipa_pad_feedback() ? .on : .off) { _ in
        nx2ipa_pad_feedback_set(!nx2ipa_pad_feedback())
    }
    let quit = UIAction(title: "Quit game", attributes: .destructive) { _ in requestExit() }
    return [UIMenu(title: "Controller layout", options: .singleSelection, children: layouts), feedback, quit]
}

@MainActor
private func requestExit() {
    let windows = UIApplication.shared.connectedScenes
        .compactMap { $0 as? UIWindowScene }
        .filter { $0.activationState == .foregroundActive }
        .flatMap(\.windows)
    guard let root = windows.first(where: \.isKeyWindow)?.rootViewController else { return }
    if root.presentedViewController is UIAlertController { return }
    nx2ipa_overlay_reset()
    let prompt = UIAlertController(title: "Quit game?", message: "Unsaved progress will be lost. Open the app again to restart.", preferredStyle: .alert)
    prompt.addAction(UIAlertAction(title: "Cancel", style: .cancel))
    prompt.addAction(UIAlertAction(title: "Quit", style: .destructive) { _ in
        nx2ipa_overlay_active(false)
        let cover = UILabel()
        cover.frame = root.view.bounds
        cover.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        cover.backgroundColor = .black
        cover.textColor = .white
        cover.textAlignment = .center
        cover.text = "Closing game…"
        root.view.addSubview(cover)
        nx2ipa_stop()
    })
    let show = {
        guard root.view.window?.windowScene?.activationState == .foregroundActive,
              root.presentedViewController == nil else { return }
        root.present(prompt, animated: true)
    }
    if root.presentedViewController == nil { show() }
    else { root.dismiss(animated: true, completion: show) }
}
