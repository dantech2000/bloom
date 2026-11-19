import BloomCore
import Foundation

/// The downloads' index and records as older builds wrote them: the kind
/// and the collection type of an item, and the kind of a segment, were the
/// server's text ("Movie", "tvshows", "Intro"); now they are the bridge's
/// enums. `upgrade` rewrites that text into the enums' coding, so a file
/// of either build decodes.
enum StoredFormat {
    /// The value in `data`, written by this build or an older one; whether
    /// it had the older form (and should be written again).
    nonisolated static func decode<T: Decodable>(_ type: T.Type, from data: Data) -> (value: T, old: Bool)? {
        if let value = try? JSONDecoder().decode(type, from: data) { return (value, false) }
        if let value = try? JSONDecoder().decode(type, from: upgrade(data)) { return (value, true) }
        return nil
    }

    /// An index (a list of entries) or one record, with the text of the
    /// older form in the enums' coding; anything else as it is.
    nonisolated static func upgrade(_ data: Data) -> Data {
        guard let json = try? JSONSerialization.jsonObject(with: data) else { return data }
        let upgraded: Any = (json as? [Any]).map { $0.map(entry) } ?? entry(json)
        return (try? JSONSerialization.data(withJSONObject: upgraded)) ?? data
    }

    private nonisolated static func entry(_ value: Any) -> Any {
        guard var entry = value as? [String: Any] else { return value }
        if var item = entry["item"] as? [String: Any] {
            if let text = item["kind"] as? String { item["kind"] = coded(mediaKind(name: text)) }
            if let text = item["collectionType"] as? String { item["collectionType"] = coded(collectionType(name: text)) }
            entry["item"] = item
        }
        if let segments = entry["segments"] as? [Any] {
            entry["segments"] = segments.map { value -> Any in
                guard var segment = value as? [String: Any], let text = segment["kind"] as? String else { return value }
                segment["kind"] = coded(segmentKind(name: text))
                return segment
            }
        }
        return entry
    }

    /// A value as the decoder of this build reads it.
    private nonisolated static func coded(_ value: some Encodable) -> Any {
        guard let data = try? JSONEncoder().encode(value),
              let json = try? JSONSerialization.jsonObject(with: data, options: .fragmentsAllowed) else { return NSNull() }
        return json
    }
}
