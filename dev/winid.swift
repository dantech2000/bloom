import CoreGraphics
import Foundation
let name = CommandLine.arguments[1]
// Optional second argument: only windows of this process id.
let pid = CommandLine.arguments.count > 2 ? Int(CommandLine.arguments[2]) : nil
if let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] {
    for w in list {
        // With a process id, the id decides; the name of the owner is the
        // name of the app (src/brand.rs) or of the bare binary.
        guard let owner = w[kCGWindowOwnerName as String] as? String, pid != nil || owner == name,
              let layer = w[kCGWindowLayer as String] as? Int, layer == 0 || layer == 3,
              let id = w[kCGWindowNumber as String] as? Int else { continue }
        if let pid, (w[kCGWindowOwnerPID as String] as? Int) != pid { continue }
        print(id); break
    }
}
