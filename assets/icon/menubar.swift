// Draws Docket's menu-bar glyph (a clipboard with two ticks) as a black
// template image, 44×44 px, and writes raw RGBA bytes.
// swift assets/icon/menubar.swift assets/icon/menubar.rgba /tmp/menubar.png
import AppKit
let n = 44
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: n * 4, bitsPerPixel: 32)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor.clear.set(); NSRect(x: 0, y: 0, width: n, height: n).fill()
NSColor.black.set()
// y grows upwards in AppKit.
let board = NSBezierPath(roundedRect: NSRect(x: 8, y: 3, width: 28, height: 34), xRadius: 4, yRadius: 4)
board.lineWidth = 3.2; board.stroke()
NSBezierPath(roundedRect: NSRect(x: 14, y: 33, width: 16, height: 8), xRadius: 2.5, yRadius: 2.5).fill()
for y in [24.0, 13.0] {
  let tick = NSBezierPath(); tick.lineWidth = 3; tick.lineCapStyle = .round; tick.lineJoinStyle = .round
  tick.move(to: NSPoint(x: 13, y: y)); tick.line(to: NSPoint(x: 16.5, y: y - 3.5)); tick.line(to: NSPoint(x: 22, y: y + 3)); tick.stroke()
  let line = NSBezierPath(); line.lineWidth = 3; line.lineCapStyle = .round
  line.move(to: NSPoint(x: 25.5, y: y)); line.line(to: NSPoint(x: 31, y: y)); line.stroke()
}
NSGraphicsContext.restoreGraphicsState()
let data = Data(bytes: rep.bitmapData!, count: n * n * 4)
try! data.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
