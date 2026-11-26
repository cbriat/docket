// Draws Docket's menu-bar glyph (the app icon's "D" page with a folded
// corner and three lines) as a black template image, 44×44 px, and writes
// raw RGBA bytes.
// swift assets/icon/menubar.swift assets/icon/menubar.rgba /tmp/menubar.png
import AppKit
let n = 44
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: n * 4, bitsPerPixel: 32)!
NSGraphicsContext.saveGraphicsState()
let ctx = NSGraphicsContext(bitmapImageRep: rep)!
NSGraphicsContext.current = ctx
NSColor.clear.set(); NSRect(x: 0, y: 0, width: n, height: n).fill()
NSColor.black.set()
// y grows upwards in AppKit. The page: left edge, round right side, and the
// top-left corner cut off where it is folded.
let page = NSBezierPath()
page.move(to: NSPoint(x: 8, y: 4))
page.line(to: NSPoint(x: 22, y: 4))
page.curve(to: NSPoint(x: 38, y: 22), controlPoint1: NSPoint(x: 33, y: 4), controlPoint2: NSPoint(x: 38, y: 12))
page.curve(to: NSPoint(x: 22, y: 40), controlPoint1: NSPoint(x: 38, y: 32), controlPoint2: NSPoint(x: 33, y: 40))
page.line(to: NSPoint(x: 17, y: 40))
page.line(to: NSPoint(x: 8, y: 31))
page.close()
page.fill()
// The folded flap, apart from the page by a thin gap.
ctx.compositingOperation = .clear
let gap = NSBezierPath(); gap.lineWidth = 2.2; gap.lineCapStyle = .round
gap.move(to: NSPoint(x: 17.8, y: 41)); gap.line(to: NSPoint(x: 17.8, y: 30.2)); gap.line(to: NSPoint(x: 7, y: 30.2)); gap.stroke()
// Three lines of text, cut out of the page.
for (y, end) in [(23.0, 29.0), (17.0, 31.0), (11.0, 25.0)] {
  let line = NSBezierPath(); line.lineWidth = 2.6; line.lineCapStyle = .round
  line.move(to: NSPoint(x: 13.5, y: y)); line.line(to: NSPoint(x: end, y: y)); line.stroke()
}
ctx.compositingOperation = .sourceOver
let flap = NSBezierPath()
flap.move(to: NSPoint(x: 16.2, y: 40.5)); flap.line(to: NSPoint(x: 16.2, y: 31.8)); flap.line(to: NSPoint(x: 7.5, y: 31.8)); flap.close()
flap.fill()
NSGraphicsContext.restoreGraphicsState()
let data = Data(bytes: rep.bitmapData!, count: n * n * 4)
try! data.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
