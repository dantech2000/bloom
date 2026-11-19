import BloomCore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The branding: the sign-in page's disclaimer, the web client's CSS, the
/// splash screen switch, and the splash image (upload from Photos or Files,
/// Remove). The server writes the image's place into the branding, so an
/// image change takes the server's branding and keeps the unsaved text.
struct BrandingPage: View {
    @Environment(AppModel.self) private var model
    @State private var branding: Branding?
    @State private var disclaimer = ""
    @State private var css = ""
    @State private var splash = false
    @State private var error: String?
    @State private var saving = false
    @State private var changingImage = false
    @State private var choosePhoto = false
    @State private var photo: PhotosPickerItem?
    @State private var chooseFile = false
    @State private var confirmRemove = false
    /// Changes with each image change, so the preview loads the image again.
    @State private var revision = 0

    /// The types the server takes, as the Mac's file dialog has them.
    private static let imageTypes: [UTType] = [.png, .jpeg, .webP, .gif]
    /// Largest image the page sends (the Mac's limit).
    private static let maxBytes = 10 * 1024 * 1024

    private var dirty: Bool {
        guard let branding else { return false }
        return branding.loginDisclaimer != disclaimer || branding.customCss != css || branding.splashscreenEnabled != splash
    }

    var body: some View {
        Group {
            if let branding {
                Form {
                    Section {
                        TextEditor(text: $disclaimer).frame(minHeight: 100)
                    } header: {
                        Text("Login disclaimer")
                    } footer: {
                        Text("A message at the bottom of the sign-in page. It can have several lines.")
                    }
                    Section {
                        TextEditor(text: $css)
                            .font(.system(.footnote, design: .monospaced))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .frame(minHeight: 180)
                    } header: {
                        Text("Custom CSS")
                    } footer: {
                        Text("Styles the web client of the server loads. A mistake here changes how the web client looks.")
                    }
                    Section {
                        Toggle("Enable the splash screen", isOn: $splash)
                        preview(branding)
                        Text(state(branding))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("branding.splash.state")
                        Menu {
                            Button("Photo Library", systemImage: "photo.on.rectangle") { choosePhoto = true }
                            Button("Files", systemImage: "folder") { chooseFile = true }
                            // A UI test has no picker to drive: it uploads an image of its own.
                            if ProcessInfo.processInfo.environment["BLOOM_TEST_SPLASH"] == "1" {
                                Button("Test image") { upload(Self.testImage(), mime: "image/png") }
                            }
                        } label: {
                            // The whole row takes the tap, not the words alone.
                            Label("Upload image…", systemImage: "square.and.arrow.up")
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .contentShape(.rect)
                        }
                        .disabled(changingImage)
                        .accessibilityIdentifier("branding.upload")
                        if branding.customSplash {
                            Button("Remove image…", role: .destructive) { confirmRemove = true }
                                .disabled(changingImage)
                                .accessibilityIdentifier("branding.remove")
                        }
                    } header: {
                        Text("Splash screen")
                    } footer: {
                        Text("The image clients show while they load. PNG, JPEG, WebP or GIF, up to 10 MB.")
                    }
                }
                .photosPicker(isPresented: $choosePhoto, selection: $photo, matching: .images)
                .onChange(of: photo) { _, item in
                    guard let item else { return }
                    photo = nil
                    Task { await uploadPhoto(item) }
                }
                .fileImporter(isPresented: $chooseFile, allowedContentTypes: Self.imageTypes) { result in
                    switch result {
                    case .success(let url): uploadFile(url)
                    case .failure(let error): model.say(Words.error(error))
                    }
                }
                .confirmationDialog("Remove the splash screen?", isPresented: $confirmRemove, titleVisibility: .visible) {
                    Button("Remove", role: .destructive) { remove() }
                } message: {
                    Text("The custom image is deleted from the server. Clients show the default image again.")
                }
            } else if let error {
                ContentUnavailableView {
                    Label("Branding could not load", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await load() } }
                }
            } else {
                ProgressView()
            }
        }
        .navigationTitle("Branding")
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if saving { ProgressView() } else { Button("Save") { Task { await save() } }.disabled(!dirty) }
            }
        }
        .task { if branding == nil { await load() } }
    }

    private func show(_ b: Branding) {
        branding = b
        disclaimer = b.loginDisclaimer
        css = b.customCss
        splash = b.splashscreenEnabled
    }

    /// The image as clients load it, over a note that stays when the server
    /// has none or it cannot load. The server serves the image only while
    /// the splash screen is on (404 otherwise, custom image or not).
    private func preview(_ b: Branding) -> some View {
        ZStack {
            Text(b.splashscreenEnabled ? "No image" : "No preview while the splash screen is off")
                .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center).padding()
            if let url = URL(string: b.splashUrl + "?tag=\(revision)") {
                AsyncImage(url: url) { phase in
                    if let image = phase.image { image.resizable().scaledToFill() } else { Color.clear }
                }
            }
            if changingImage { ProgressView() }
        }
        .frame(maxWidth: .infinity)
        .aspectRatio(16 / 9, contentMode: .fit)
        .background(Color.black.opacity(0.35))
        .clipShape(.rect(cornerRadius: 12))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(b.customSplash ? "Custom splash image" : "Default splash image")
        .accessibilityIdentifier("branding.splash.preview")
    }

    private func state(_ b: Branding) -> String {
        switch (b.customSplash, b.splashscreenEnabled) {
        case (true, true): "A custom image is set. Clients show it while they load."
        case (true, false): "A custom image is set. The splash screen is off, so clients do not show it."
        case (false, true): "No custom image. Clients show the default image of the server."
        case (false, false): "No custom image. The splash screen is off."
        }
    }

    /// A photo goes as it is when the server takes its type; a HEIC or RAW
    /// photo goes as a JPEG.
    private func uploadPhoto(_ item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self) else {
            model.say("The photo could not be read.")
            return
        }
        if let type = item.supportedContentTypes.first(where: { t in Self.imageTypes.contains { t.conforms(to: $0) } }),
           let mime = type.preferredMIMEType {
            upload(data, mime: mime)
        } else if let jpeg = UIImage(data: data)?.jpegData(compressionQuality: 0.9) {
            upload(jpeg, mime: "image/jpeg")
        } else {
            model.say("Choose a PNG, JPEG, WebP or GIF image.")
        }
    }

    private func uploadFile(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let type = UTType(filenameExtension: url.pathExtension), Self.imageTypes.contains(where: { type.conforms(to: $0) }),
              let mime = type.preferredMIMEType else {
            model.say("Choose a PNG, JPEG, WebP or GIF image.")
            return
        }
        // The size first: a large file is not read to learn it.
        if let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size > Self.maxBytes {
            model.say("The image is larger than 10 MB.")
            return
        }
        guard let data = try? Data(contentsOf: url) else {
            model.say("The file could not be read.")
            return
        }
        upload(data, mime: mime)
    }

    private func upload(_ data: Data, mime: String) {
        guard data.count <= Self.maxBytes else {
            model.say("The image is larger than 10 MB.")
            return
        }
        changeImage("The splash screen is set.") { try await $0.adminUploadSplash(image: data, mime: mime) }
    }

    private func remove() {
        changeImage("The splash screen is removed.") { try await $0.adminRemoveSplash() }
    }

    /// Runs a change of the image and takes the server's branding after it;
    /// the text the user has not saved stays in the fields.
    private func changeImage(_ done: String, _ work: @escaping (Server) async throws -> Branding) {
        guard !changingImage, let server = model.server else { return }
        changingImage = true
        Task {
            defer { changingImage = false }
            do {
                branding = try await work(server)
                revision += 1
                model.say(done)
            } catch {
                model.say(Words.error(error))
            }
        }
    }

    /// The image of `BLOOM_TEST_SPLASH`: a plain 16:9 PNG.
    private static func testImage() -> Data {
        let size = CGSize(width: 320, height: 180)
        return UIGraphicsImageRenderer(size: size, format: { let f = UIGraphicsImageRendererFormat(); f.scale = 1; return f }()).pngData { context in
            UIColor(white: 0.2, alpha: 1).setFill()
            context.fill(CGRect(origin: .zero, size: size))
        }
    }

    private func load() async {
        do { show(try await model.adminServer().adminBranding()); error = nil } catch { self.error = Words.error(error) }
    }

    private func save() async {
        guard let branding else { return }
        saving = true
        defer { saving = false }
        do {
            show(try await model.adminServer().adminSaveBranding(original: branding.original, loginDisclaimer: disclaimer, customCss: css, splashscreenEnabled: splash))
            model.say("Branding saved")
        } catch {
            model.say(Words.error(error))
        }
    }
}
