import UIKit
import QuartzCore
import AVFAudio

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    // The display link belongs to this layer for the lifetime of the guest.
    // UIKit can discard a background scene and reconnect it in the same process.
    var runningGame: GameView?
    weak var gameScene: SceneDelegate?
    var lifecycle = GameLifecycle()

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        NotificationCenter.default.addObserver(self, selector: #selector(audioInterrupted(_:)),
                                               name: AVAudioSession.interruptionNotification,
                                               object: AVAudioSession.sharedInstance())
        return true
    }

    @objc private func audioInterrupted(_ notification: Notification) {
        guard let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw) else { return }
        // AVAudioSession posts on the main thread. Record the event in order
        // with scene callbacks, even when the game has no attached scene.
        lifecycle.interruption(began: type == .began)
        gameScene?.applyActive()
    }

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
    private var game: GameView?
    private weak var app: AppDelegate?
    private var audioAvailable = false
    private var audioRetry: Task<Void, Never>?
    private var retryDelay = 1
    private var updatingActivity = false

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard session.role == .windowApplication, let scene = scene as? UIWindowScene,
              let app = UIApplication.shared.delegate as? AppDelegate else { return }
        let window = UIWindow(windowScene: scene)
        let controller = UIViewController()
        let view = app.runningGame ?? GameView(frame: window.bounds)
        controller.view = view
        window.rootViewController = controller
        self.window = window
        window.makeKeyAndVisible()
        let maximumFPS = UInt32(scene.screen.maximumFramesPerSecond)
        if app.runningGame == nil {
            if let failure = start(view, maximumFPS: maximumFPS) {
                let label = UILabel(frame: view.bounds.insetBy(dx: 24, dy: 24))
                label.autoresizingMask = [.flexibleWidth, .flexibleHeight]
                label.numberOfLines = 0
                label.textColor = .white
                label.text = failure
                view.addSubview(label)
                return
            }
            app.runningGame = view
            view.addSubview(view.controls)
        } else {
            nxkit_set_display_rate(maximumFPS)
        }
        game = view
        self.app = app
        app.gameScene = self
        view.setNeedsLayout()
        do {
            let audio = AVAudioSession.sharedInstance()
            try audio.setCategory(.playback)
            try audio.setPreferredSampleRate(48_000)
        } catch {
            NSLog("nxkit audio session: %@", error.localizedDescription)
        }
        setActive(scene.activationState == .foregroundActive)
    }

    private func start(_ view: GameView, maximumFPS: UInt32) -> String? {
        var error = [CChar](repeating: 0, count: 4096)
        let started = Bundle.main.bundlePath.withCString { bundle in
            NSHomeDirectory().withCString { home in
                error.withUnsafeMutableBufferPointer { buffer in
                    nxkit_start(Unmanaged.passUnretained(view.metalLayer).toOpaque(), bundle, home,
                                maximumFPS, buffer.baseAddress, buffer.count)
                }
            }
        }
        return started ? nil : error.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
    }
    func sceneDidBecomeActive(_ scene: UIScene) {
        setActive(true)
    }
    func sceneWillResignActive(_ scene: UIScene) { setActive(false) }
    func sceneDidEnterBackground(_ scene: UIScene) { enterBackground() }
    func sceneDidDisconnect(_ scene: UIScene) {
        enterBackground()
        if app?.gameScene === self { app?.gameScene = nil }
        // Release the scene's ownership. The next scene attaches the same view
        // and Metal layer to its new window, with the guest and its saves intact.
        window?.rootViewController = nil
        window = nil
        game = nil
    }
    private func setActive(_ active: Bool) {
        guard let app, app.gameScene === self else { return }
        app.lifecycle.setForeground(active)
        applyActive()
    }
    private func enterBackground() {
        guard let app, app.gameScene === self else { return }
        app.lifecycle.enterBackground()
        applyActive()
    }
    fileprivate func applyActive() {
        guard let game, let app, app.gameScene === self else { return }
        // A session call can deliver an interruption before it returns. Finish
        // the current native transition before applying the new state.
        if updatingActivity {
            DispatchQueue.main.async { [weak self] in self?.applyActive() }
            return
        }
        updatingActivity = true
        defer { updatingActivity = false }
        let audio = AVAudioSession.sharedInstance()
        if !app.lifecycle.canRequestAudio {
            UIApplication.shared.isIdleTimerDisabled = false
            game.controls.isUserInteractionEnabled = false
            game.controls.reset()
            audioRetry?.cancel()
            audioRetry = nil
            retryDelay = 1
            nxkit_set_activity(false, false)
            if audioAvailable {
                audioAvailable = false
                do { try audio.setActive(false, options: .notifyOthersOnDeactivation) }
                catch { NSLog("nxkit audio deactivation: %@", error.localizedDescription) }
            }
            return
        }
        if !audioAvailable && audioRetry == nil {
            do {
                try audio.setActive(true)
                audioAvailable = true
                app.lifecycle.audioActivated()
                retryDelay = 1
            } catch {
                let delay = retryDelay
                if delay == 1 { NSLog("nxkit audio activation: %@", error.localizedDescription) }
                retryDelay = min(delay * 2, 30)
                audioRetry = Task { [weak self] in
                    do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                    guard let self, !Task.isCancelled else { return }
                    self.audioRetry = nil
                    self.applyActive()
                }
            }
        }
        let active = app.lifecycle.gameActive
        UIApplication.shared.isIdleTimerDisabled = active
        game.controls.isUserInteractionEnabled = active
        nxkit_set_activity(active, audioAvailable)
    }
}

@MainActor
final class GameView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    let controls = Controls()
    override init(frame: CGRect) { super.init(frame: frame); backgroundColor = .black }
    required init?(coder: NSCoder) { fatalError("Programmatic view") }
    override func layoutSubviews() {
        super.layoutSubviews()
        let scale = window?.screen.scale ?? 1
        metalLayer.contentsScale = scale
        metalLayer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
        controls.frame = bounds
    }
}
