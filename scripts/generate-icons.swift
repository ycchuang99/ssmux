// Run on macOS: swift scripts/generate-icons.swift
import AppKit

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let assets = root.appendingPathComponent("assets")
let iconset = root.appendingPathComponent("target/SSMux.iconset")
try FileManager.default.createDirectory(at: assets, withIntermediateDirectories: true)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

func symbol(_ context: CGContext, color: CGColor) {
    context.setStrokeColor(color)
    context.setFillColor(color)
    context.setLineWidth(5)
    context.setLineCap(.round)
    context.setLineJoin(.round)
    context.addPath(CGPath(roundedRect: CGRect(x: 14, y: 24, width: 72, height: 52),
                           cornerWidth: 11, cornerHeight: 11, transform: nil))
    context.strokePath()
    context.move(to: CGPoint(x: 27, y: 41))
    context.addLine(to: CGPoint(x: 37, y: 50))
    context.addLine(to: CGPoint(x: 27, y: 59))
    context.strokePath()
    context.setLineWidth(3.5)
    context.move(to: CGPoint(x: 47, y: 50))
    context.addLine(to: CGPoint(x: 58, y: 50))
    context.move(to: CGPoint(x: 69, y: 39))
    context.addLine(to: CGPoint(x: 60, y: 39))
    context.addQuadCurve(to: CGPoint(x: 56, y: 43), control: CGPoint(x: 56, y: 39))
    context.addLine(to: CGPoint(x: 56, y: 57))
    context.addQuadCurve(to: CGPoint(x: 60, y: 61), control: CGPoint(x: 56, y: 61))
    context.addLine(to: CGPoint(x: 69, y: 61))
    context.strokePath()
    for y in [39.0, 61.0] {
        context.fillEllipse(in: CGRect(x: 67, y: y - 3, width: 6, height: 6))
    }
}

func render(size: Int, template: Bool) throws -> (Data, Data) {
    let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                            bytesPerRow: size * 4, space: CGColorSpaceCreateDeviceRGB(),
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    context.scaleBy(x: CGFloat(size) / 100, y: CGFloat(size) / 100)
    if template {
        symbol(context, color: CGColor(gray: 0, alpha: 1))
    } else {
        context.saveGState()
        let tile = CGPath(roundedRect: CGRect(x: 5, y: 5, width: 90, height: 90),
                          cornerWidth: 20, cornerHeight: 20, transform: nil)
        context.addPath(tile)
        context.clip()
        let colors = [CGColor(red: 0.025, green: 0.10, blue: 0.22, alpha: 1),
                      CGColor(red: 0.04, green: 0.38, blue: 0.48, alpha: 1)]
        let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
                                  colors: colors as CFArray, locations: [0, 1])!
        context.drawLinearGradient(gradient, start: CGPoint(x: 20, y: 10),
                                   end: CGPoint(x: 85, y: 100),
                                   options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
        context.restoreGState()
        context.setStrokeColor(CGColor(gray: 1, alpha: 0.15))
        context.setLineWidth(0.5)
        context.addPath(tile)
        context.strokePath()
        context.translateBy(x: 5, y: 5)
        context.scaleBy(x: 0.9, y: 0.9)
        symbol(context, color: CGColor(red: 0.82, green: 1, blue: 0.96, alpha: 1))
    }
    let image = context.makeImage()!
    let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
    let pixels = Data(bytes: context.data!, count: size * size * 4)
    return (png, pixels)
}

let (template, pixels) = try render(size: 36, template: true)
try template.write(to: assets.appendingPathComponent("menu-template.png"))
try pixels.write(to: assets.appendingPathComponent("menu-template.rgba"))
let (preview, _) = try render(size: 256, template: false)
try preview.write(to: assets.appendingPathComponent("app-icon-preview.png"))
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let (png, _) = try render(size: size * scale, template: false)
        let suffix = scale == 2 ? "@2x" : ""
        try png.write(to: iconset.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
        if size * scale == 1024 {
            try png.write(to: assets.appendingPathComponent("app-icon.png"))
        }
    }
}
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", assets.appendingPathComponent("AppIcon.icns").path]
try task.run()
task.waitUntilExit()
if task.terminationStatus != 0 {
    throw NSError(domain: "SSMuxIconGeneration", code: Int(task.terminationStatus),
                  userInfo: [NSLocalizedDescriptionKey: "iconutil could not build AppIcon.icns"])
}
