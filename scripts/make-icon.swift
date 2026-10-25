import AppKit
import Foundation

// A small, code-native icon: pickup bag and availability indicator.
let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
for (name, size) in [("icon_16x16",16),("icon_16x16@2x",32),("icon_32x32",32),("icon_32x32@2x",64),("icon_128x128",128),("icon_128x128@2x",256),("icon_256x256",256),("icon_256x256@2x",512),("icon_512x512",512),("icon_512x512@2x",1024)] {
    let bitmap = NSBitmapImageRep(bitmapDataPlanes:nil, pixelsWide:size, pixelsHigh:size, bitsPerSample:8, samplesPerPixel:4, hasAlpha:true, isPlanar:false, colorSpaceName:.deviceRGB, bytesPerRow:0, bitsPerPixel:0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep:bitmap)
    let scale = NSAffineTransform()
    scale.scale(by:CGFloat(size)/1024)
    scale.concat()
    let tile = NSBezierPath(roundedRect:NSRect(x:48,y:48,width:928,height:928), xRadius:208,yRadius:208)
    NSGradient(starting:NSColor(srgbRed:0.36,green:0.43,blue:0.59,alpha:1), ending:NSColor(srgbRed:0.19,green:0.24,blue:0.37,alpha:1))!.draw(in:tile,angle:90)
    NSColor.white.withAlphaComponent(0.97).setStroke()
    let bag = NSBezierPath(roundedRect:NSRect(x:288,y:236,width:448,height:434),xRadius:42,yRadius:42)
    bag.lineWidth=37
    bag.stroke()
    let handle = NSBezierPath()
    handle.move(to:NSPoint(x:407,y:644))
    handle.line(to:NSPoint(x:407,y:705))
    handle.curve(to:NSPoint(x:617,y:705),controlPoint1:NSPoint(x:407,y:844),controlPoint2:NSPoint(x:617,y:844))
    handle.line(to:NSPoint(x:617,y:644))
    handle.lineWidth=37
    handle.lineCapStyle = .round
    handle.stroke()
    NSColor(srgbRed:0.30,green:0.73,blue:0.49,alpha:1).setFill()
    NSBezierPath(ovalIn:NSRect(x:654,y:186,width:152,height:152)).fill()
    NSColor.white.setStroke()
    let check=NSBezierPath()
    check.move(to:NSPoint(x:696,y:261));check.line(to:NSPoint(x:721,y:237));check.line(to:NSPoint(x:766,y:288))
    check.lineWidth=15;check.lineCapStyle = .round;check.lineJoinStyle = .round;check.stroke()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using:.png,properties:[:])!.write(to:output.appendingPathComponent(name+".png"))
}
