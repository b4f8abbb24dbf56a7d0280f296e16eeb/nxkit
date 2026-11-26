import UIKit
import QuartzCore
import AVFAudio

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    // The guest and its Metal layer live for the process, not one scene connection.
    var runningGame: GameView?
    weak var gameScene: SceneDelegate?

    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Game", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}

@MainActor
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private weak var app: AppDelegate?
    private var foreground = false
    private var interrupted = false
    private var interruptionObserver: NSObjectProtocol?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard session.role == .windowApplication, let scene = scene as? UIWindowScene,
              let app = UIApplication.shared.delegate as? AppDelegate else { return }
        self.app = app
        let window = UIWindow(windowScene: scene)
        let controller = UIViewController()
        let view = app.runningGame ?? GameView(frame: window.bounds)
        controller.view = view
        window.rootViewController = controller
        self.window = window
        window.makeKeyAndVisible()
        do {
            let audio = AVAudioSession.sharedInstance()
            try audio.setCategory(.playback)
            try audio.setPreferredSampleRate(48_000)
        } catch {
            NSLog("nxkit audio session: %@", error.localizedDescription)
        }
        interruptionObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
        ) { [weak self] notification in
            guard let rawType = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  let type = AVAudioSession.InterruptionType(rawValue: rawType) else { return }
            let began = type == .began
            Task { @MainActor [weak self] in
                guard let self, self.app?.gameScene === self else { return }
                self.interrupted = began
                self.applyActive()
            }
        }
        var error = [CChar](repeating: 0, count: 4096)
        let started = app.runningGame != nil || Bundle.main.bundlePath.withCString { bundle in
            NSHomeDirectory().withCString { home in
                error.withUnsafeMutableBufferPointer { buffer in
                    nxkit_start(Unmanaged.passUnretained(view.metalLayer).toOpaque(), bundle, home,
                                UInt32(scene.screen.maximumFramesPerSecond), buffer.baseAddress, buffer.count)
                }
            }
        }
        if started {
            app.runningGame = view
            app.gameScene = self
            setActive(scene.activationState == .foregroundActive)
        } else {
            let label = UILabel(frame: view.bounds.insetBy(dx: 24, dy: 24))
            label.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            label.numberOfLines = 0
            label.textColor = .white
            label.text = error.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
            view.addSubview(label)
        }
    }
    func sceneDidBecomeActive(_ scene: UIScene) {
        interrupted = false
        setActive(true)
    }
    func sceneWillResignActive(_ scene: UIScene) { setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) {
        setActive(false)
        if app?.gameScene === self { app?.gameScene = nil }
        // Disconnect is not process exit: a new scene must reuse the running guest.
        window?.rootViewController = nil
        window = nil
        if let interruptionObserver { NotificationCenter.default.removeObserver(interruptionObserver) }
        interruptionObserver = nil
    }
    private func setActive(_ active: Bool) {
        guard app?.gameScene === self else { return }
        foreground = active
        applyActive()
    }
    private func applyActive() {
        var active = foreground && !interrupted
        if !active { nxkit_set_active(false) }
        do {
            try AVAudioSession.sharedInstance().setActive(active, options: active ? [] : .notifyOthersOnDeactivation)
        } catch {
            NSLog("nxkit audio activation: %@", error.localizedDescription)
            // Do not resume RemoteIO or rendering without an active audio session.
            // A later foreground/interruption-end event can retry activation.
            active = false
            nxkit_set_active(false)
        }
        UIApplication.shared.isIdleTimerDisabled = active
        if active { nxkit_set_active(true) }
    }
}

@MainActor
final class GameView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    override init(frame: CGRect) { super.init(frame: frame); backgroundColor = .black }
    required init?(coder: NSCoder) { fatalError("Programmatic view") }
    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.screen.scale ?? 1
        metalLayer.contentsScale = scale
        metalLayer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
    }
}
