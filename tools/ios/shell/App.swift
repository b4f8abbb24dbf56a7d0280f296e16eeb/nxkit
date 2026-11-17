import UIKit

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Game", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
    func applicationWillTerminate(_ application: UIApplication) { nxkit_stop() }
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
        startGame(scene: scene, controller: controller)
    }
    private func startGame(scene: UIWindowScene, controller: UIViewController) {
        let view = MetalView(frame: window?.bounds ?? scene.coordinateSpace.bounds)
        controller.view = view
        var error = [CChar](repeating: 0, count: 4096)
        let started = Bundle.main.bundlePath.withCString { bundle in
            NSHomeDirectory().withCString { home in
                error.withUnsafeMutableBufferPointer { buffer in
                    nxkit_start(Unmanaged.passUnretained(view.metalLayer).toOpaque(), bundle, home,
                                 UInt32(clamping: scene.screen.maximumFramesPerSecond), buffer.baseAddress, buffer.count)
                }
            }
        }
        guard started else {
            let message = error.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
            showError(message.isEmpty ? "The native session could not start." : message, in: controller)
            return
        }
        view.installOverlay()
        metalView = view
        connected = true
        view.setNeedsLayout()
        view.layoutIfNeeded()
        setActive(scene.activationState == .foregroundActive)
    }
    func sceneDidBecomeActive(_ scene: UIScene) { setActive(true) }
    func sceneWillResignActive(_ scene: UIScene) { setActive(false) }
    func sceneDidEnterBackground(_ scene: UIScene) { setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) {
        setActive(false)
        if connected { nxkit_stop() }
        connected = false
        metalView = nil
        window = nil
    }
    private func setActive(_ active: Bool) {
        guard connected else { return }
        metalView?.active = active
        nxkit_set_active(active)
    }
    private func showError(_ message: String, in controller: UIViewController) {
        let label = UILabel()
        label.text = message
        label.textColor = .white
        label.numberOfLines = 0
        label.translatesAutoresizingMaskIntoConstraints = false
        let container = UIView()
        container.backgroundColor = .black
        container.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: container.safeAreaLayoutGuide.leadingAnchor, constant: 24),
            label.trailingAnchor.constraint(equalTo: container.safeAreaLayoutGuide.trailingAnchor, constant: -24),
            label.centerYAnchor.constraint(equalTo: container.safeAreaLayoutGuide.centerYAnchor)
        ])
        controller.view = container
        NSLog("nxkit: %@", message)
    }
}
