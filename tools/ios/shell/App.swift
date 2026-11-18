import UIKit
import QuartzCore
import AVFAudio

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
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
    private var foreground = false
    private var interrupted = false
    private var interruptionObserver: NSObjectProtocol?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        let controller = UIViewController()
        let view = GameView(frame: window.bounds)
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
            let began = (notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue
            let options = notification.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.interrupted = began
                if began || AVAudioSession.InterruptionOptions(rawValue: options).contains(.shouldResume) {
                    self.applyActive()
                }
            }
        }
        var error = [CChar](repeating: 0, count: 4096)
        let started = Bundle.main.bundlePath.withCString { bundle in
            NSHomeDirectory().withCString { home in
                error.withUnsafeMutableBufferPointer { buffer in
                    nxkit_start(Unmanaged.passUnretained(view.metalLayer).toOpaque(), bundle, home,
                                UInt32(scene.screen.maximumFramesPerSecond), buffer.baseAddress, buffer.count)
                }
            }
        }
        if started {
            game = view
            view.addSubview(view.controls)
            view.setNeedsLayout()
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
    func sceneDidBecomeActive(_ scene: UIScene) { setActive(true) }
    func sceneWillResignActive(_ scene: UIScene) { setActive(false) }
    func sceneDidDisconnect(_ scene: UIScene) {
        setActive(false)
        if game != nil { nxkit_stop() }
        game = nil
        if let interruptionObserver { NotificationCenter.default.removeObserver(interruptionObserver) }
        interruptionObserver = nil
    }
    private func setActive(_ active: Bool) {
        foreground = active
        applyActive()
    }
    private func applyActive() {
        guard let game else { return }
        let active = foreground && !interrupted
        if !active { nxkit_set_active(false) }
        do {
            try AVAudioSession.sharedInstance().setActive(active, options: active ? [] : .notifyOthersOnDeactivation)
        } catch {
            NSLog("nxkit audio activation: %@", error.localizedDescription)
        }
        game.controls.reset()
        game.controls.isUserInteractionEnabled = active
        nxkit_set_active(active)
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
        if bounds.width > 0 && bounds.height > 0 {
            nxkit_resize(UInt32(metalLayer.drawableSize.width), UInt32(metalLayer.drawableSize.height))
        }
        controls.frame = bounds
    }
}
