// Spike S2 (specs/ios-native/plan.md): mpv through MPVUI with the
// AVFoundation sample-buffer output, which is what iOS Picture in Picture
// needs. A small bench: a stream, subtitles on or off, and hardware or
// software decoding, so a run on the phone shows which ones play in PiP.
//
// 2026-11-14, first run on the phone: Apple's "advanced" stream stopped
// with "no color space for native CI overlay" at 0:10. MPVUI 0.10.0 draws
// subtitles and on-screen text over the frame with Core Image, and needs
// the frame's colour space, which that stream's frames do not carry.
import AVFoundation
import MPVUI
import SwiftUI

@main
struct PiPSpikeApp: App {
    var body: some Scene {
        WindowGroup {
            Bench()
                .preferredColorScheme(.dark)
        }
    }
}

/// One way to play, as the bench chose it.
struct Variant: Hashable {
    var stream = 0
    var subtitles = false
    var softwareDecoding = false

    static let streams: [(name: String, url: String)] = [
        ("Apple advanced (subtitles)", "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_ts/master.m3u8"),
        ("Apple basic 16:9", "https://devstreaming-cdn.apple.com/videos/streaming/examples/bipbop_16x9/bipbop_16x9_variant.m3u8"),
        ("Mux test film", "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8"),
    ]

    var options: [String: String] {
        var options: [String: String] = [:]
        if !subtitles {
            options["sid"] = "no"
            options["osd-level"] = "0"
        }
        if softwareDecoding { options["hwdec"] = "no" }
        return options
    }
}

struct Bench: View {
    @State private var variant = Variant()

    var body: some View {
        VStack(spacing: 14) {
            // A new player for each choice.
            SpikePlayer(variant: variant).id(variant)
            Form {
                Picker("Stream", selection: $variant.stream) {
                    ForEach(Variant.streams.indices, id: \.self) { Text(Variant.streams[$0].name).tag($0) }
                }
                Toggle("Subtitles and on-screen text", isOn: $variant.subtitles)
                Toggle("Software decoding", isOn: $variant.softwareDecoding)
            }
            .frame(height: 200)
            .scrollContentBackground(.hidden)
        }
    }
}

struct SpikePlayer: View {
    let variant: Variant
    @State private var player: MPVPlayer

    init(variant: Variant) {
        self.variant = variant
        _player = State(initialValue: MPVPlayer(configuration: .init(additionalOptions: variant.options, videoOutput: .sampleBuffer)))
    }

    var body: some View {
        VStack(spacing: 12) {
            MPVVideoPlayer(player: player)
                .aspectRatio(16 / 9, contentMode: .fit)
                .background(.black)
            HStack {
                Button(player.isPaused ? "Play" : "Pause") { player.togglePlayback() }
                Button("PiP") { player.pictureInPicture.toggle() }
                    .accessibilityIdentifier("spike.pip")
                    .disabled(!player.pictureInPicture.isPossible)
            }
            .buttonStyle(.bordered)
            Text("state \(String(describing: player.state)) · \(player.position.formatted(.time(pattern: .minuteSecond))) / \(player.duration.formatted(.time(pattern: .minuteSecond)))")
                .font(.footnote.monospaced())
                .accessibilityIdentifier("spike.state")
            Text("PiP supported \(player.pictureInPicture.isSupported ? "yes" : "no") · possible \(player.pictureInPicture.isPossible ? "yes" : "no") · active \(player.pictureInPicture.isActive ? "yes" : "no")")
                .font(.footnote.monospaced())
                .accessibilityIdentifier("spike.pipstate")
            if let error = player.lastError {
                Text("error: \(String(describing: error))").font(.footnote).foregroundStyle(.red)
            }
        }
        .padding(.horizontal)
        .task {
            try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
            try? AVAudioSession.sharedInstance().setActive(true)
            player.pictureInPicture.allowsAutomaticStartFromInline = true
            player.load(URL(string: Variant.streams[variant.stream].url)!)
        }
        .onDisappear { player.stop() }
    }
}
