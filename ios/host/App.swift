// The UIKit host of Bloom for iOS. UIKit owns the window and the run loop;
// GPUI (libbloom_ios.a) owns everything drawn in it. The scene delegate
// starts GPUI when its window scene connects and installs the view
// controller GPUI made as the root of the window.
import AVFoundation
import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Main", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
               options: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        // The sound of a video plays like a media app: over the silent
        // switch, and it stops other audio.
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
        try? AVAudioSession.sharedInstance().setActive(true)
        gpui_ios_set_embedded()
        bloom_ios_start()
        guard let gpuiWindow = gpui_ios_get_window(),
              let controller = gpui_ios_view_controller(gpuiWindow) else {
            fatalError("GPUI opened no window")
        }
        let root = Unmanaged<UIViewController>.fromOpaque(controller).takeUnretainedValue()
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = root
        window.makeKeyAndVisible()
        self.window = window
    }

    func sceneDidBecomeActive(_ scene: UIScene) { gpui_ios_did_become_active(nil) }
    func sceneWillResignActive(_ scene: UIScene) { gpui_ios_will_resign_active(nil) }
    func sceneDidEnterBackground(_ scene: UIScene) { gpui_ios_did_enter_background(nil) }
    func sceneWillEnterForeground(_ scene: UIScene) { gpui_ios_will_enter_foreground(nil) }
}
