// poke: posts one untagged released-Option flagsChanged, to test the battery's real-input abort.
import Cocoa
let w = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 58, keyDown: false)!
w.type = .flagsChanged; w.flags = []; w.post(tap: .cghidEventTap)
