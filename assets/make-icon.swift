import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

private let canvasSize = 1024

private enum Palette {
    static let deepInk = CGColor(srgbRed: 0x18 / 255.0, green: 0x16 / 255.0, blue: 0x1A / 255.0, alpha: 1)
    static let lightInk = CGColor(srgbRed: 0x2A / 255.0, green: 0x26 / 255.0, blue: 0x2C / 255.0, alpha: 1)
    static let cream = CGColor(srgbRed: 0xE9 / 255.0, green: 0xE3 / 255.0, blue: 0xD5 / 255.0, alpha: 1)
    static let haze = CGColor(srgbRed: 0x9E / 255.0, green: 0x96 / 255.0, blue: 0x89 / 255.0, alpha: 1)
    static let amber = CGColor(srgbRed: 0xC8 / 255.0, green: 0x9C / 255.0, blue: 0x62 / 255.0, alpha: 1)
}

private func makeContext() -> CGContext {
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
    guard let context = CGContext(
        data: nil,
        width: canvasSize,
        height: canvasSize,
        bitsPerComponent: 8,
        bytesPerRow: canvasSize * 4,
        space: colorSpace,
        bitmapInfo: bitmapInfo.rawValue
    ) else {
        fatalError("Unable to create the CoreGraphics bitmap context")
    }
    context.setAllowsAntialiasing(true)
    context.setShouldAntialias(true)
    context.interpolationQuality = .high
    return context
}

private func drawIcon(in context: CGContext) {
    context.clear(CGRect(x: 0, y: 0, width: canvasSize, height: canvasSize))

    // An 824-point rounded field centered on the 1024-point canvas leaves the
    // published macOS icon-grid padding transparent.
    let fieldRect = CGRect(x: 100, y: 100, width: 824, height: 824)
    let field = CGPath(roundedRect: fieldRect, cornerWidth: 188, cornerHeight: 188, transform: nil)
    context.addPath(field)
    context.setFillColor(Palette.deepInk)
    context.fillPath()

    context.saveGState()
    context.addPath(field)
    context.clip()

    // The first haboku layer: a restrained, organic light-ink wash. It stays
    // close to the ground value so the decisive cream stroke remains dominant.
    let wash = CGMutablePath()
    wash.move(to: CGPoint(x: 128, y: 534))
    wash.addCurve(
        to: CGPoint(x: 490, y: 746),
        control1: CGPoint(x: 226, y: 690),
        control2: CGPoint(x: 354, y: 780)
    )
    wash.addCurve(
        to: CGPoint(x: 893, y: 604),
        control1: CGPoint(x: 637, y: 711),
        control2: CGPoint(x: 772, y: 635)
    )
    wash.addLine(to: CGPoint(x: 893, y: 472))
    wash.addCurve(
        to: CGPoint(x: 502, y: 612),
        control1: CGPoint(x: 757, y: 510),
        control2: CGPoint(x: 631, y: 581)
    )
    wash.addCurve(
        to: CGPoint(x: 128, y: 392),
        control1: CGPoint(x: 346, y: 653),
        control2: CGPoint(x: 226, y: 548)
    )
    wash.closeSubpath()
    context.addPath(wash)
    context.setFillColor(Palette.lightInk)
    context.fillPath()

    // The second layer: one continuous brush silhouette. The lower-left entry
    // is broad and wet; the upper-right exit accelerates into a narrow point.
    let stroke = CGMutablePath()
    stroke.move(to: CGPoint(x: 182, y: 286))
    stroke.addCurve(
        to: CGPoint(x: 314, y: 214),
        control1: CGPoint(x: 179, y: 230),
        control2: CGPoint(x: 246, y: 184)
    )
    stroke.addCurve(
        to: CGPoint(x: 548, y: 445),
        control1: CGPoint(x: 397, y: 269),
        control2: CGPoint(x: 469, y: 368)
    )
    stroke.addCurve(
        to: CGPoint(x: 835, y: 790),
        control1: CGPoint(x: 648, y: 541),
        control2: CGPoint(x: 727, y: 657)
    )
    stroke.addCurve(
        to: CGPoint(x: 774, y: 724),
        control1: CGPoint(x: 816, y: 769),
        control2: CGPoint(x: 796, y: 747)
    )
    stroke.addCurve(
        to: CGPoint(x: 512, y: 508),
        control1: CGPoint(x: 689, y: 634),
        control2: CGPoint(x: 607, y: 568)
    )
    stroke.addCurve(
        to: CGPoint(x: 306, y: 350),
        control1: CGPoint(x: 426, y: 454),
        control2: CGPoint(x: 359, y: 400)
    )
    stroke.addCurve(
        to: CGPoint(x: 182, y: 286),
        control1: CGPoint(x: 260, y: 307),
        control2: CGPoint(x: 217, y: 285)
    )
    stroke.closeSubpath()
    context.addPath(stroke)
    context.setFillColor(Palette.cream)
    context.fillPath()

    // Two narrow ground-colored channels expose the dry, "flying white" tail
    // without breaking the gesture into multiple strokes.
    let dryChannelOne = CGMutablePath()
    dryChannelOne.move(to: CGPoint(x: 557, y: 492))
    dryChannelOne.addCurve(
        to: CGPoint(x: 780, y: 732),
        control1: CGPoint(x: 644, y: 555),
        control2: CGPoint(x: 724, y: 650)
    )
    dryChannelOne.addCurve(
        to: CGPoint(x: 567, y: 501),
        control1: CGPoint(x: 718, y: 658),
        control2: CGPoint(x: 642, y: 570)
    )
    dryChannelOne.closeSubpath()
    context.addPath(dryChannelOne)
    context.setFillColor(Palette.deepInk)
    context.fillPath()

    let dryChannelTwo = CGMutablePath()
    dryChannelTwo.move(to: CGPoint(x: 642, y: 582))
    dryChannelTwo.addCurve(
        to: CGPoint(x: 808, y: 761),
        control1: CGPoint(x: 706, y: 631)
        , control2: CGPoint(x: 762, y: 700)
    )
    dryChannelTwo.addCurve(
        to: CGPoint(x: 650, y: 592),
        control1: CGPoint(x: 758, y: 705),
        control2: CGPoint(x: 704, y: 643)
    )
    dryChannelTwo.closeSubpath()
    context.addPath(dryChannelTwo)
    context.setFillColor(Palette.lightInk)
    context.fillPath()

    // One small "here and now" accent, attached to the exiting tip.
    context.setFillColor(Palette.amber)
    context.fillEllipse(in: CGRect(x: 814, y: 765, width: 17, height: 17))

    context.restoreGState()
}

private func writePNG(_ image: CGImage, to outputURL: URL) throws {
    guard let destination = CGImageDestinationCreateWithURL(
        outputURL as CFURL,
        UTType.png.identifier as CFString,
        1,
        nil
    ) else {
        throw NSError(domain: "haboku.icon", code: 1, userInfo: [NSLocalizedDescriptionKey: "Unable to create PNG destination"])
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw NSError(domain: "haboku.icon", code: 2, userInfo: [NSLocalizedDescriptionKey: "Unable to finalize PNG"])
    }
}

guard CommandLine.arguments.count == 2 else {
    fputs("usage: make-icon <output.png>\n", stderr)
    exit(64)
}

let context = makeContext()
drawIcon(in: context)
guard let image = context.makeImage() else {
    fputs("Unable to create icon image\n", stderr)
    exit(1)
}

do {
    try writePNG(image, to: URL(fileURLWithPath: CommandLine.arguments[1]))
} catch {
    fputs("\(error)\n", stderr)
    exit(1)
}
