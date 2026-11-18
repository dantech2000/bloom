import BloomCore
import MPVUI
import SwiftUI

/// The small player over the pages (the Mac's small window, inventory
/// §3.1): the picture, play and pause, Skip or Next, back to full screen,
/// and close. A drag puts it in another corner. Leaving Bloom hands the
/// picture to the system's Picture in Picture.
struct MiniPlayer: View {
    @Environment(AppModel.self) private var app
    let model: PlayerModel
    @State private var corner = Corner.bottomTrailing
    @State private var drag: CGSize = .zero
    @State private var controls = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    enum Corner: CaseIterable { case topLeading, topTrailing, bottomLeading, bottomTrailing }

    var body: some View {
        GeometryReader { box in
            let width = min(box.size.width * 0.6, 320)
            card(width: width)
                .frame(width: width, height: width * 9 / 16)
                .clipShape(.rect(cornerRadius: 16))
                // No shadow (DESIGN.md): the light edge lifts it off the page.
                .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(Color.ink.opacity(0.2), lineWidth: 1) }
                .position(place(width: width, in: box.size))
                .offset(drag)
                .gesture(DragGesture()
                    .onChanged { drag = $0.translation }
                    .onEnded { end in
                        let point = place(width: width, in: box.size)
                        let dropped = CGPoint(x: point.x + end.predictedEndTranslation.width, y: point.y + end.predictedEndTranslation.height)
                        withAnimation(reduceMotion ? nil : .snappy(duration: 0.25)) {
                            corner = Corner.allCases.min { a, b in
                                distance(place(width: width, in: box.size, at: a), dropped) < distance(place(width: width, in: box.size, at: b), dropped)
                            } ?? corner
                            drag = .zero
                        }
                    })
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("mini.player")
    }

    @ViewBuilder
    private func card(width: CGFloat) -> some View {
        ZStack {
            if let engine = model.engine {
                MPVVideoPlayer(player: engine)
            } else {
                Color.black
            }
            if controls {
                // Glass over the picture, not a dark slab: each control on
                // its own pane.
                GlassEffectContainer {
                    ZStack {
                        HStack(spacing: 6) {
                            Button(model.state.paused ? "Play" : "Pause", systemImage: model.state.paused ? "play.fill" : "pause.fill") {
                                model.togglePause()
                            }
                            .toolTarget()
                            .accessibilityIdentifier("mini.pause")
                            if let segment = model.skippable {
                                Button(PlayerView.skipLabel(segment.kind), systemImage: "forward.frame.fill") { model.skipSegment() }
                                    .toolTarget()
                            } else if model.nextItem != nil {
                                Button("Next", systemImage: "forward.end.fill") { Task { await model.playNext() } }
                                    .toolTarget()
                            }
                        }
                        .font(.title3)
                        .padding(.horizontal, 8)
                        .glassEffect(.regular.interactive(), in: .capsule)
                        VStack {
                            HStack {
                                Button("Close", systemImage: "xmark") { app.closePlayer() }
                                    .buttonStyle(DiscButtonStyle(size: 32))
                                    .accessibilityIdentifier("mini.close")
                                Spacer()
                                Button("Full screen", systemImage: "arrow.up.left.and.arrow.down.right") { app.expandPlayer() }
                                    .buttonStyle(DiscButtonStyle(size: 32))
                                    .accessibilityIdentifier("mini.expand")
                            }
                            Spacer()
                        }
                        .font(.footnote.weight(.semibold))
                        .padding(2)
                    }
                    .labelStyle(.iconOnly)
                }
                .transition(.opacity)
            }
            // How far it is, as a thin line along the bottom; it stays
            // when the controls go.
            ProgressLine(progress: Float(min(1, model.state.position / max(model.state.duration, 1))))
                .frame(maxHeight: .infinity, alignment: .bottom)
        }
        .foregroundStyle(Color.ink)
        .contentShape(.rect)
        // The double tap first, or the single tap takes both taps.
        .onTapGesture(count: 2) { app.expandPlayer() }
        .onTapGesture { withAnimation(.easeOut(duration: 0.15)) { controls.toggle() } }
    }

    private func place(width: CGFloat, in size: CGSize, at: Corner? = nil) -> CGPoint {
        let height = width * 9 / 16
        let margin: CGFloat = 16
        // Above the tab bar at the bottom, under the status bar at the top.
        let (top, bottom) = (CGFloat(60), CGFloat(100))
        let left = margin + width / 2, right = size.width - margin - width / 2
        let upper = top + height / 2, lower = size.height - bottom - height / 2
        switch at ?? corner {
        case .topLeading: return CGPoint(x: left, y: upper)
        case .topTrailing: return CGPoint(x: right, y: upper)
        case .bottomLeading: return CGPoint(x: left, y: lower)
        case .bottomTrailing: return CGPoint(x: right, y: lower)
        }
    }

    private func distance(_ a: CGPoint, _ b: CGPoint) -> CGFloat { hypot(a.x - b.x, a.y - b.y) }
}
