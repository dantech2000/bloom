import BloomCore
import SwiftUI

/// A track as a person reads it: "English", "English (SDH)", "English
/// (image)", "French · Commentary", with the format as a quiet detail
/// ("Dolby Digital+ 5.1 · Default"). Never the codec as the name
/// ("HDMV_PGS_SUBTITLE").
struct TrackName: Equatable {
    let name: String
    let detail: String?

    /// The name and the detail on one line, for a place with one line.
    var full: String { [name, detail].compactMap { $0 }.joined(separator: " · ") }

    enum Kind { case audio, subtitle }

    init(name: String, detail: String?) {
        self.name = name
        self.detail = detail
    }

    init(kind: Kind, language: String?, codec: String?, title: String?, channels: Int? = nil, layout: String? = nil,
         forced: Bool = false, hearingImpaired: Bool = false, isDefault: Bool = false) {
        let languageName = Self.language(language)
        let words = Self.words(title)
        let image = kind == .subtitle && Self.isImage(codec)
        var marks: [String] = []
        if kind == .subtitle {
            if forced || words.contains("forced") { marks.append("Forced") }
            if hearingImpaired || words.contains("sdh") || words.contains("cc") { marks.append("SDH") }
            if image { marks.append("image") }
        }
        let extra = Self.meaningful(title, language: languageName, codec: codec)
        var name = languageName ?? extra ?? "Unknown language"
        if languageName != nil, let extra { name += " · \(extra)" }
        if !marks.isEmpty { name += " (\(marks.joined(separator: ", ")))" }
        var detail: [String] = []
        let format = Self.codec(codec)
        if kind == .audio {
            let sound = Self.channels(channels, layout: layout)
            if let joined = [format, sound].compactMap({ $0 }).joined(separator: " ").nilIfEmpty { detail.append(joined) }
        } else if let format {
            detail.append(format)
        }
        if isDefault { detail.append("Default") }
        self.name = name
        self.detail = detail.isEmpty ? nil : detail.joined(separator: " · ")
    }

    init(_ track: TrackInfo) {
        self.init(kind: track.kind == .audio ? .audio : .subtitle, language: track.language, codec: track.codec, title: track.title,
                  channels: track.channels.map(Int.init), layout: track.channelLayout, forced: track.isForced,
                  hearingImpaired: track.isHearingImpaired, isDefault: track.isDefault)
    }

    // MARK: Words

    /// A language code ("eng", "fre", "pt-BR") as its name in the user's
    /// language; nil for none or "undetermined".
    static func language(_ code: String?) -> String? {
        guard var code = code?.trimmingCharacters(in: .whitespaces), !code.isEmpty else { return nil }
        let lower = code.lowercased()
        if ["und", "undefined", "unknown", "zxx", "mis", "mul"].contains(lower) { return nil }
        // ISO 639-2 B codes, which Foundation does not know.
        let bibliographic = ["fre": "fra", "ger": "deu", "chi": "zho", "dut": "nld", "cze": "ces", "gre": "ell", "per": "fas",
                             "rum": "ron", "slo": "slk", "alb": "sqi", "arm": "hye", "baq": "eus", "bur": "mya", "geo": "kat",
                             "ice": "isl", "mac": "mkd", "mao": "mri", "may": "msa", "tib": "bod", "wel": "cym"]
        if let t = bibliographic[lower] { code = t }
        return Locale.current.localizedString(forLanguageCode: code) ?? Locale.current.localizedString(forIdentifier: code) ?? code
    }

    /// An image subtitle: drawn pictures, not text (PGS, VobSub, DVB).
    static func isImage(_ codec: String?) -> Bool {
        guard let codec = codec?.lowercased() else { return false }
        return ["pgssub", "pgs", "hdmv_pgs_subtitle", "sup", "dvdsub", "dvd_subtitle", "vobsub", "dvbsub", "dvb_subtitle", "xsub"].contains(codec)
    }

    /// The format in its common name; nil for an image subtitle (the name
    /// says "image" already) and for none.
    static func codec(_ codec: String?) -> String? {
        guard let raw = codec?.lowercased(), !raw.isEmpty, !isImage(raw) else { return nil }
        let names = ["aac": "AAC", "ac3": "Dolby Digital", "eac3": "Dolby Digital+", "truehd": "Dolby TrueHD", "dts": "DTS",
                     "dca": "DTS", "flac": "FLAC", "opus": "Opus", "mp3": "MP3", "vorbis": "Vorbis", "alac": "ALAC",
                     "pcm_s16le": "PCM", "pcm_s24le": "PCM", "subrip": "SRT", "srt": "SRT", "ass": "ASS", "ssa": "SSA",
                     "webvtt": "WebVTT", "vtt": "WebVTT", "mov_text": "Text"]
        return names[raw] ?? raw.uppercased()
    }

    /// "5.1", "7.1", "Stereo", "Mono".
    static func channels(_ count: Int?, layout: String?) -> String? {
        switch count {
        case 1: return "Mono"
        case 2: return "Stereo"
        case 6: return "5.1"
        case 8: return "7.1"
        case let n?: return layout?.capitalized ?? "\(n) channels"
        case nil: return layout?.capitalized
        }
    }

    /// The lowercased words of a title.
    private static func words(_ title: String?) -> Set<String> {
        guard let title else { return [] }
        let parts = title.lowercased().components(separatedBy: CharacterSet.alphanumerics.union(CharacterSet(charactersIn: ".+")).inverted)
        return Set(parts.filter { !$0.isEmpty })
    }

    /// The words of a title that say more than the language and the
    /// format ("Commentary", "Signs & Songs"), as the title; nil when it
    /// only repeats them ("English - SUBRIP - Default", "English [SDH]").
    static func meaningful(_ title: String?, language: String?, codec: String?) -> String? {
        guard let title = title?.trimmingCharacters(in: .whitespaces), !title.isEmpty else { return nil }
        var known: Set<String> = ["default", "forced", "sdh", "cc", "external", "full", "subtitles", "subtitle", "subs", "sub",
                                  "und", "undefined", "unknown", "stereo", "mono", "5.1", "7.1", "2.0", "2.1", "hearing", "impaired",
                                  "srt", "subrip", "pgs", "pgssub", "ass", "ssa", "vtt", "webvtt", "text", "hdmv", "dvd", "dvb",
                                  "vobsub", "dvdsub", "mov", "dolby", "digital", "digital+", "aac", "ac3", "eac3", "dts", "truehd",
                                  "flac", "opus", "mp3", "atmos", "hd", "ma", "track", "audio", "image", "sup", "and", "plus"]
        known.formUnion(words(language))
        known.formUnion(words(codec))
        known.formUnion(words(Self.codec(codec)))
        // A bare code of three letters is a language ("eng").
        let left = words(title).filter { word in
            !known.contains(word) && !(word.count == 3 && Locale.current.localizedString(forLanguageCode: word) != nil)
        }
        // A number alone ("Track 3", "2") says nothing either.
        guard left.contains(where: { $0.contains(where: \.isLetter) }) else { return nil }
        return title
    }
}

/// What the file is: resolution and range, the codecs, the sound, the
/// subtitle languages and the size, as a quiet pane of rows (the Mac's
/// media info, in plain words).
struct FileSection: View {
    let file: MediaFile

    var body: some View {
        let rows = Self.rows(file)
        if !rows.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("File").font(.subheadline.weight(.semibold)).foregroundStyle(Color.ink.opacity(0.6))
                    .accessibilityAddTraits(.isHeader)
                VStack(spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                        if index > 0 {
                            Rectangle().fill(Color.ink.opacity(0.1)).frame(height: 0.5).padding(.leading, 14)
                        }
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(row.0).foregroundStyle(Color.ink.opacity(0.6))
                                .frame(width: 84, alignment: .leading)
                            Text(row.1).fixedSize(horizontal: false, vertical: true)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .font(.subheadline)
                        .padding(.horizontal, 14).padding(.vertical, 11)
                        // One text per row, "Video: 4K · HEVC": a bare
                        // "Video" would match the player's video view in
                        // the tests (the page stays under the player), and
                        // a hidden child still showed to them.
                        .accessibilityRepresentation { Text("\(row.0): \(row.1)") }
                    }
                }
                .background(Color.ink.opacity(0.06), in: .rect(cornerRadius: Metrics.controlCorner))
            }
            .frame(maxWidth: 480, alignment: .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("detail.file")
        }
    }

    /// The label and the value of each row, for the rows the file has.
    static func rows(_ file: MediaFile) -> [(String, String)] {
        var rows: [(String, String)] = []
        if let video = file.video {
            let parts = [resolution(video), range(video.rangeType), videoCodec(video.codec)].compactMap { $0 }
            if !parts.isEmpty { rows.append(("Video", parts.joined(separator: " · "))) }
        }
        let audios = file.tracks.filter { $0.kind == .audio }
        if let main = audios.first(where: \.isDefault) ?? audios.first {
            // "English · Dolby Digital+ 5.1", and how many others.
            let sound = [TrackName.codec(main.codec), TrackName.channels(main.channels.map(Int.init), layout: main.channelLayout)]
                .compactMap { $0 }.joined(separator: " ")
            var text = [TrackName.language(main.language), sound.isEmpty ? nil : sound].compactMap { $0 }.joined(separator: " · ")
            if text.isEmpty { text = TrackName(main).name }
            if audios.count > 1 { text += audios.count == 2 ? ", and 1 more track" : ", and \(audios.count - 1) more tracks" }
            rows.append(("Audio", text))
        }
        var languages: [String] = []
        for track in file.tracks where track.kind == .subtitle {
            let name = TrackName.language(track.language) ?? "Unknown language"
            if !languages.contains(name) { languages.append(name) }
        }
        if !languages.isEmpty { rows.append(("Subtitles", languages.joined(separator: ", "))) }
        let size = file.sizeBytes.map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file) }
        let container = file.container?.split(separator: ",").first.map { $0.uppercased() }
        if let size = [size, container].compactMap({ $0 }).joined(separator: " · ").nilIfEmpty {
            rows.append(("Size", size))
        }
        return rows
    }

    /// "4K", "1080p", "720p", "SD", by the frame's size (a film cropped to
    /// 3840×1600 is still 4K).
    static func resolution(_ video: VideoInfo) -> String? {
        let width = Int(video.width ?? 0), height = Int(video.height ?? 0)
        guard width > 0 || height > 0 else { return nil }
        if width >= 3200 || height >= 2000 { return "4K" }
        if width >= 1800 || height >= 1000 { return "1080p" }
        if width >= 1200 || height >= 700 { return "720p" }
        return "SD"
    }

    static func range(_ type: String?) -> String? {
        guard let type = type?.uppercased(), !type.isEmpty, type != "SDR", type != "UNKNOWN" else { return nil }
        if type.hasPrefix("DOVI") { return "Dolby Vision" }
        switch type {
        case "HDR10PLUS": return "HDR10+"
        case "HDR10": return "HDR10"
        case "HLG": return "HLG"
        default: return "HDR"
        }
    }

    static func videoCodec(_ codec: String?) -> String? {
        guard let codec = codec?.lowercased(), !codec.isEmpty else { return nil }
        let names = ["hevc": "HEVC", "h265": "HEVC", "h264": "H.264", "avc": "H.264", "av1": "AV1", "vp9": "VP9",
                     "vp8": "VP8", "mpeg2video": "MPEG-2", "mpeg4": "MPEG-4", "vc1": "VC-1"]
        return names[codec] ?? codec.uppercased()
    }
}

/// "Ends at 9:42 PM": now plus what is left of the item, kept current
/// each minute.
struct EndsAt: View {
    let remainingSecs: Int64

    var body: some View {
        TimelineView(.everyMinute) { context in
            let end = context.date.addingTimeInterval(TimeInterval(remainingSecs))
            Text("Ends at \(end.formatted(date: .omitted, time: .shortened))")
                .font(.footnote).monospacedDigit()
                .foregroundStyle(Color.ink.opacity(0.6))
        }
        .accessibilityIdentifier("detail.ends-at")
    }
}
