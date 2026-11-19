import Foundation
import Observation

/// The settings this device keeps (spec §8.6). Server-side preferences
/// (languages, home sections) come in a later build; these are the app's.
@Observable
final class Preferences {
    static let shared = Preferences()

    private let store = UserDefaults.standard

    /// The cap of a stream in bits per second; 0 plays the file as it is
    /// ("Play the file as it is. Quality is lowered only when the user
    /// asks.").
    var maxBitrate: UInt64 {
        didSet { store.set(Int(maxBitrate), forKey: "maxBitrate") }
    }

    /// The cap of a stream while the phone streams over cellular (spec Q11);
    /// 0 is "Original", no limit of its own. On cellular the lower of this
    /// and the cap above applies.
    var cellularMaxBitrate: UInt64 {
        didSet { store.set(Int(cellularMaxBitrate), forKey: "cellularMaxBitrate") }
    }

    /// Skips an intro (and recap) without a tap, once per segment.
    var autoSkipIntro: Bool {
        didSet { store.set(autoSkipIntro, forKey: "autoSkipIntro") }
    }

    /// Skips the credits without a tap.
    var autoSkipCredits: Bool {
        didSet { store.set(autoSkipCredits, forKey: "autoSkipCredits") }
    }

    /// Downloads over a cellular connection (off: Wi-Fi only, spec §8.5).
    var downloadOverCellular: Bool {
        didSet { store.set(downloadOverCellular, forKey: "downloadOverCellular") }
    }

    /// Downloads that run at once, 1 or 2 (the session's connections per
    /// server; the Mac's default is 1).
    var downloadsAtOnce: Int {
        didSet { store.set(downloadsAtOnce, forKey: "downloadsAtOnce") }
    }

    /// The space all downloads may take, in GB; 0 for no limit.
    var downloadLimitGB: Int {
        didSet { store.set(downloadLimitGB, forKey: "downloadLimitGB") }
    }

    /// Other devices of this user may control this one (Play on).
    var allowRemoteControl: Bool {
        didSet { store.set(allowRemoteControl, forKey: "allowRemoteControl") }
    }

    /// The look of subtitles in this app (the Mac's "Appearance in this
    /// app"): 0 or "" keeps mpv's default.
    var subtitleScale: Double {
        didSet { store.set(subtitleScale, forKey: "subtitleScale") }
    }
    var subtitleColor: String {
        didSet { store.set(subtitleColor, forKey: "subtitleColor") }
    }
    /// 0 default, 1 a dark box, 2 no box.
    var subtitleBox: Int {
        didSet { store.set(subtitleBox, forKey: "subtitleBox") }
    }
    /// Place from the top in percent; 0 default.
    var subtitlePosition: Int {
        didSet { store.set(subtitlePosition, forKey: "subtitlePosition") }
    }

    /// Jellyfin Enhanced features turned off on this device alone (the Mac's
    /// profile menu switches), by key.
    var enhancedOff: Set<String> {
        didSet { store.set(Array(enhancedOff), forKey: "enhancedOff") }
    }

    /// Quality tags on the cards on this device: nil follows the user's
    /// Jellyfin Enhanced setting (the Mac's profile menu switch).
    var qualityTags: Bool? {
        didSet { store.set(qualityTags, forKey: "qualityTags") }
    }

    /// "Auto" quality lowers a file the link cannot carry (the Mac's
    /// adaptive quality, on by default).
    var lowerOnSlowLink: Bool {
        didSet { store.set(lowerOnSlowLink, forKey: "lowerOnSlowLink") }
    }

    private init() {
        qualityTags = store.object(forKey: "qualityTags") as? Bool
        lowerOnSlowLink = store.object(forKey: "lowerOnSlowLink") as? Bool ?? true
        enhancedOff = Set(store.stringArray(forKey: "enhancedOff") ?? [])
        subtitleScale = store.double(forKey: "subtitleScale")
        subtitleColor = store.string(forKey: "subtitleColor") ?? ""
        subtitleBox = store.integer(forKey: "subtitleBox")
        subtitlePosition = store.integer(forKey: "subtitlePosition")
        allowRemoteControl = store.object(forKey: "allowRemoteControl") as? Bool ?? true
        downloadOverCellular = store.bool(forKey: "downloadOverCellular")
        downloadsAtOnce = min(max(store.integer(forKey: "downloadsAtOnce"), 1), 2)
        downloadLimitGB = max(0, store.integer(forKey: "downloadLimitGB"))
        maxBitrate = UInt64(max(0, store.integer(forKey: "maxBitrate")))
        cellularMaxBitrate = UInt64(max(0, store.integer(forKey: "cellularMaxBitrate")))
        autoSkipIntro = store.bool(forKey: "autoSkipIntro")
        autoSkipCredits = store.bool(forKey: "autoSkipCredits")
    }

    /// The cap for a request: none for Auto.
    var cap: UInt64? { maxBitrate == 0 ? nil : maxBitrate }

    /// The cellular limit: none for Original.
    var cellularCap: UInt64? { cellularMaxBitrate == 0 ? nil : cellularMaxBitrate }
}
