import UIKit

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        StartupDiagnostics.shared.install()
        return true
    }
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Game", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
    func applicationWillTerminate(_ application: UIApplication) { nx2ipa_stop() }
}

@MainActor
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var metalView: MetalView?
    private var connected = false
    // The shared Rust kernel cannot reset its detached threads/OnceLocks.
    private static var used = false

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        let controller = UIViewController()
        window.rootViewController = controller
        self.window = window
        window.makeKeyAndVisible()
        guard !Self.used else {
            showError("This process already started a game session. Relaunch the app to restart.", in: controller)
            return
        }
        Self.used = true
        if StartupDiagnostics.shared.previousStartupInterrupted,
           let log = StartupDiagnostics.shared.previous {
            showRecovery(log, in: controller) { [weak self] in
                self?.startGame(scene: scene, controller: controller)
            }
        } else {
            startGame(scene: scene, controller: controller)
        }
    }
    private func startGame(scene: UIWindowScene, controller: UIViewController) {
        StartupDiagnostics.shared.begin()
        let view = MetalView(frame: window?.bounds ?? scene.coordinateSpace.bounds)
        controller.view = view
        StartupDiagnostics.shared.record("calling native session startup")
        var error = [CChar](repeating: 0, count: 4096)
        let started = Bundle.main.bundlePath.withCString { bundle in
            NSHomeDirectory().withCString { home in
                error.withUnsafeMutableBufferPointer { buffer in
                    nx2ipa_start(Unmanaged.passUnretained(view.metalLayer).toOpaque(), bundle, home,
                                 UInt32(clamping: scene.screen.maximumFramesPerSecond), buffer.baseAddress, buffer.count)
                }
            }
        }
        guard started else {
            let message = error.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
            StartupDiagnostics.shared.record("native session returned an error: \(message)")
            StartupDiagnostics.shared.finishCapture()
            showError(message.isEmpty ? "The native session could not start." : message, in: controller)
            return
        }
        StartupDiagnostics.shared.record("native session returned successfully; installing controls")
        view.installOverlay()
        metalView = view
        connected = true
        view.setNeedsLayout()
        view.layoutIfNeeded()
        setActive(scene.activationState == .foregroundActive)
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) {
            StartupDiagnostics.shared.checkpoint()
        }
    }
    func sceneDidBecomeActive(_ scene: UIScene) { setActive(true) }
    func sceneWillResignActive(_ scene: UIScene) { setActive(false) }
    func sceneDidEnterBackground(_ scene: UIScene) { setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) {
        setActive(false)
        if connected { nx2ipa_stop() }
        connected = false
        metalView = nil
        window = nil
    }
    private func setActive(_ active: Bool) {
        guard connected else { return }
        metalView?.active = active
        nx2ipa_set_active(active)
    }
    private func showRecovery(_ log: URL, in controller: UIViewController, start: @escaping @MainActor () -> Void) {
        showError("The previous launch ended before its startup checkpoint. This can also happen if you close the app during startup. You can share its diagnostic log before retrying.", in: controller, log: log, retry: start)
    }
    private func showError(_ message: String, in controller: UIViewController, log: URL? = nil, retry: (@MainActor () -> Void)? = nil) {
        let container = UIView()
        container.backgroundColor = .black
        let label = UILabel()
        label.text = message
        label.textColor = .white
        label.numberOfLines = 0
        let stack = UIStackView(arrangedSubviews: [label])
        stack.axis = .vertical
        stack.spacing = 20
        stack.translatesAutoresizingMaskIntoConstraints = false
        if let log = log ?? StartupDiagnostics.shared.current,
           FileManager.default.fileExists(atPath: log.path) {
            let warning = UILabel()
            warning.text = "Logs may include local paths and game diagnostic messages. Review them before sharing. Nothing is uploaded automatically."
            warning.textColor = .lightGray
            warning.numberOfLines = 0
            stack.addArrangedSubview(warning)
            let button = UIButton(type: .system)
            button.setTitle("Share launch log", for: .normal)
            button.addAction(UIAction { [weak controller, weak button] _ in
                guard let controller, let button else { return }
                StartupDiagnostics.shared.share(log, from: controller, source: button)
            }, for: .touchUpInside)
            stack.addArrangedSubview(button)
        }
        if let retry {
            let button = UIButton(type: .system)
            button.setTitle("Try launching again", for: .normal)
            button.addAction(UIAction { _ in retry() }, for: .touchUpInside)
            stack.addArrangedSubview(button)
        }
        let scroll = UIScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(scroll)
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: container.safeAreaLayoutGuide.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.safeAreaLayoutGuide.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: container.safeAreaLayoutGuide.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: container.safeAreaLayoutGuide.bottomAnchor),
            stack.widthAnchor.constraint(equalTo: scroll.frameLayoutGuide.widthAnchor, constant: -48),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -24),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 24),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -24)
        ])
        controller.view = container
        NSLog("nx2ipa: %@", message)
    }
}
