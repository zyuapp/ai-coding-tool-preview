import CoreGraphics
import Foundation
// Prints the window number of the first on-screen normal window owned by a process name or pid.
let key = CommandLine.arguments[1]
let pid = Int32(key)
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowLayer as String] as? Int) == 0 {
  let matches = pid != nil
    ? (w[kCGWindowOwnerPID as String] as? Int32) == pid
    : (w[kCGWindowOwnerName as String] as? String) == key
  if matches { print(w[kCGWindowNumber as String]!); break }
}
