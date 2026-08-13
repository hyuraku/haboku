import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

private let designSize: CGFloat = 1024

private enum DetailLevel: String {
    case simple
    case detailed
}

private enum Palette {
    static let deepInk = CGColor(srgbRed: 0x18 / 255.0, green: 0x16 / 255.0, blue: 0x1A / 255.0, alpha: 1)
    static let cream = CGColor(srgbRed: 0xE9 / 255.0, green: 0xE3 / 255.0, blue: 0xD5 / 255.0, alpha: 1)
    // Icon-only values derived from cream. UI color tokens remain unchanged.
    static let wash = CGColor(srgbRed: 0xE9 / 255.0, green: 0xE3 / 255.0, blue: 0xD5 / 255.0, alpha: 0.24)
    static let rim = CGColor(srgbRed: 0xE9 / 255.0, green: 0xE3 / 255.0, blue: 0xD5 / 255.0, alpha: 0.20)
}

private func makeContext(pixelSize: Int) -> CGContext {
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
    guard let context = CGContext(
        data: nil,
        width: pixelSize,
        height: pixelSize,
        bitsPerComponent: 8,
        bytesPerRow: pixelSize * 4,
        space: colorSpace,
        bitmapInfo: bitmapInfo.rawValue
    ) else {
        fatalError("Unable to create the CoreGraphics bitmap context")
    }
    context.setAllowsAntialiasing(true)
    context.setShouldAntialias(true)
    context.interpolationQuality = .high
    context.scaleBy(x: CGFloat(pixelSize) / designSize, y: CGFloat(pixelSize) / designSize)
    return context
}

private func makeSquircle(in rect: CGRect, cornerRadius: CGFloat) -> CGPath {
    // A cubic superellipse approximation. Its long handles make the corner
    // visibly continuous and squircle-like instead of a circular quarter arc.
    let handle = cornerRadius * 0.91
    let path = CGMutablePath()
    path.move(to: CGPoint(x: rect.minX + cornerRadius, y: rect.minY))
    path.addLine(to: CGPoint(x: rect.maxX - cornerRadius, y: rect.minY))
    path.addCurve(
        to: CGPoint(x: rect.maxX, y: rect.minY + cornerRadius),
        control1: CGPoint(x: rect.maxX - cornerRadius + handle, y: rect.minY),
        control2: CGPoint(x: rect.maxX, y: rect.minY + cornerRadius - handle)
    )
    path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY - cornerRadius))
    path.addCurve(
        to: CGPoint(x: rect.maxX - cornerRadius, y: rect.maxY),
        control1: CGPoint(x: rect.maxX, y: rect.maxY - cornerRadius + handle),
        control2: CGPoint(x: rect.maxX - cornerRadius + handle, y: rect.maxY)
    )
    path.addLine(to: CGPoint(x: rect.minX + cornerRadius, y: rect.maxY))
    path.addCurve(
        to: CGPoint(x: rect.minX, y: rect.maxY - cornerRadius),
        control1: CGPoint(x: rect.minX + cornerRadius - handle, y: rect.maxY),
        control2: CGPoint(x: rect.minX, y: rect.maxY - cornerRadius + handle)
    )
    path.addLine(to: CGPoint(x: rect.minX, y: rect.minY + cornerRadius))
    path.addCurve(
        to: CGPoint(x: rect.minX + cornerRadius, y: rect.minY),
        control1: CGPoint(x: rect.minX, y: rect.minY + cornerRadius - handle),
        control2: CGPoint(x: rect.minX + cornerRadius - handle, y: rect.minY)
    )
    path.closeSubpath()
    return path
}

private func drawSimpleStroke(in context: CGContext, pixelSize: Int) {
    let stroke = CGMutablePath()
    stroke.move(to: CGPoint(x: 172, y: 286))

    if pixelSize <= 16 {
        // A short, blunt-nib silhouette: roughly 5.5px at the wet entry and
        // 3px at the cut exit. It deliberately has no needle-like tail.
        stroke.addCurve(
            to: CGPoint(x: 240, y: 180),
            control1: CGPoint(x: 178, y: 216),
            control2: CGPoint(x: 204, y: 186)
        )
        stroke.addCurve(
            to: CGPoint(x: 450, y: 180),
            control1: CGPoint(x: 286, y: 112),
            control2: CGPoint(x: 406, y: 112)
        )
        stroke.addCurve(
            to: CGPoint(x: 786, y: 704),
            control1: CGPoint(x: 494, y: 212),
            control2: CGPoint(x: 650, y: 527)
        )
        stroke.addLine(to: CGPoint(x: 594, y: 704))
        stroke.addCurve(
            to: CGPoint(x: 310, y: 426),
            control1: CGPoint(x: 500, y: 614),
            control2: CGPoint(x: 390, y: 511)
        )
        stroke.addCurve(
            to: CGPoint(x: 172, y: 286),
            control1: CGPoint(x: 232, y: 426),
            control2: CGPoint(x: 178, y: 350)
        )
    } else if pixelSize <= 32 {
        // The same gesture with a longer 4.5px-floor exit at 32px.
        stroke.addCurve(
            to: CGPoint(x: 200, y: 180),
            control1: CGPoint(x: 178, y: 210),
            control2: CGPoint(x: 184, y: 184)
        )
        stroke.addCurve(
            to: CGPoint(x: 490, y: 180),
            control1: CGPoint(x: 266, y: 96),
            control2: CGPoint(x: 424, y: 96)
        )
        stroke.addCurve(
            to: CGPoint(x: 846, y: 736),
            control1: CGPoint(x: 528, y: 220),
            control2: CGPoint(x: 710, y: 590)
        )
        stroke.addLine(to: CGPoint(x: 702, y: 736))
        stroke.addCurve(
            to: CGPoint(x: 320, y: 416),
            control1: CGPoint(x: 604, y: 666),
            control2: CGPoint(x: 426, y: 492)
        )
        stroke.addCurve(
            to: CGPoint(x: 172, y: 286),
            control1: CGPoint(x: 240, y: 416),
            control2: CGPoint(x: 178, y: 350)
        )
    } else {
        // At 64px the exit can taper to 5px while retaining a smooth outline.
        stroke.addCurve(
            to: CGPoint(x: 270, y: 156),
            control1: CGPoint(x: 178, y: 216),
            control2: CGPoint(x: 220, y: 161)
        )
        stroke.addCurve(
            to: CGPoint(x: 848, y: 736),
            control1: CGPoint(x: 414, y: 150),
            control2: CGPoint(x: 736, y: 640)
        )
        stroke.addLine(to: CGPoint(x: 768, y: 752))
        stroke.addCurve(
            to: CGPoint(x: 314, y: 416),
            control1: CGPoint(x: 650, y: 638),
            control2: CGPoint(x: 422, y: 496)
        )
        stroke.addCurve(
            to: CGPoint(x: 172, y: 286),
            control1: CGPoint(x: 240, y: 416),
            control2: CGPoint(x: 178, y: 348)
        )
    }

    stroke.closeSubpath()
    context.addPath(stroke)
    context.setFillColor(Palette.cream)
    context.fillPath()
}

private func drawSimpleWash(in context: CGContext) {
    let wash = CGMutablePath()
    wash.move(to: CGPoint(x: 70, y: 534))
    wash.addCurve(
        to: CGPoint(x: 490, y: 746),
        control1: CGPoint(x: 226, y: 690),
        control2: CGPoint(x: 354, y: 780)
    )
    wash.addCurve(
        to: CGPoint(x: 954, y: 604),
        control1: CGPoint(x: 637, y: 711),
        control2: CGPoint(x: 772, y: 635)
    )
    wash.addLine(to: CGPoint(x: 954, y: 472))
    wash.addCurve(
        to: CGPoint(x: 502, y: 612),
        control1: CGPoint(x: 757, y: 510),
        control2: CGPoint(x: 631, y: 581)
    )
    wash.addCurve(
        to: CGPoint(x: 70, y: 392),
        control1: CGPoint(x: 346, y: 653),
        control2: CGPoint(x: 226, y: 548)
    )
    wash.closeSubpath()
    context.addPath(wash)
    context.setFillColor(Palette.wash)
    context.fillPath()
}

private func drawIcon(in context: CGContext, pixelSize: Int, detailLevel: DetailLevel) {
    context.clear(CGRect(x: 0, y: 0, width: designSize, height: designSize))

    // An 824-point rounded field centered on the 1024-point canvas leaves the
    // published macOS icon-grid padding transparent.
    let fieldRect = CGRect(x: 100, y: 100, width: 824, height: 824)
    let field = makeSquircle(in: fieldRect, cornerRadius: designSize * (185.4 / 1024))
    context.addPath(field)
    context.setFillColor(Palette.deepInk)
    context.fillPath()
    if detailLevel == .detailed {
        context.addPath(field)
        context.setStrokeColor(Palette.rim)
        context.setLineWidth(6)
        context.strokePath()
    }

    context.saveGState()
    context.addPath(field)
    context.clip()

    if detailLevel == .simple {
        // The wash returns only where it has enough pixels to stay visibly
        // separate from the cream core. The 16px and 32px slots remain clean.
        if pixelSize >= 64 {
            drawSimpleWash(in: context)
        }
        drawSimpleStroke(in: context, pixelSize: pixelSize)
        context.restoreGState()
        return
    }

    // The first haboku layer: a restrained, organic light-ink wash. It stays
    // close to the ground value so the decisive cream stroke remains dominant.
    let wash = CGMutablePath()
    wash.move(to: CGPoint(x: 70, y: 534))
    wash.addCurve(
        to: CGPoint(x: 490, y: 746),
        control1: CGPoint(x: 226, y: 690),
        control2: CGPoint(x: 354, y: 780)
    )
    wash.addCurve(
        to: CGPoint(x: 954, y: 604),
        control1: CGPoint(x: 637, y: 711),
        control2: CGPoint(x: 772, y: 635)
    )
    wash.addLine(to: CGPoint(x: 954, y: 472))
    wash.addCurve(
        to: CGPoint(x: 502, y: 612),
        control1: CGPoint(x: 757, y: 510),
        control2: CGPoint(x: 631, y: 581)
    )
    wash.addCurve(
        to: CGPoint(x: 70, y: 392),
        control1: CGPoint(x: 346, y: 653),
        control2: CGPoint(x: 226, y: 548)
    )
    wash.closeSubpath()
    context.addPath(wash)
    context.setFillColor(Palette.wash)
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

    if detailLevel == .detailed {
        // Ground-colored channels expose the dry, "flying white" tail at sizes
        // where those details remain coherent after rasterization.
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
            control1: CGPoint(x: 706, y: 631),
            control2: CGPoint(x: 762, y: 700)
        )
        dryChannelTwo.addCurve(
            to: CGPoint(x: 650, y: 592),
            control1: CGPoint(x: 758, y: 705),
            control2: CGPoint(x: 704, y: 643)
        )
        dryChannelTwo.closeSubpath()
        context.addPath(dryChannelTwo)
        context.setFillColor(Palette.wash)
        context.fillPath()
    }

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

guard CommandLine.arguments.count == 4,
      let pixelSize = Int(CommandLine.arguments[2]), pixelSize > 0,
      let detailLevel = DetailLevel(rawValue: CommandLine.arguments[3]) else {
    fputs("usage: make-icon <output.png> <pixel-size> <simple|detailed>\n", stderr)
    exit(64)
}

let context = makeContext(pixelSize: pixelSize)
drawIcon(in: context, pixelSize: pixelSize, detailLevel: detailLevel)
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
