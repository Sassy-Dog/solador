import SwiftUI

/// Dark green-on-black terminal palette shared across the cockpit.
enum CockpitTheme {
    static let background = Color(hex: 0x000000)
    static let panel = Color(hex: 0x050805)
    static let panelAlt = Color(hex: 0x0A0F0C)
    static let line = Color(hex: 0x13301F)
    static let green = Color(hex: 0x33D17A)
    static let greenDim = Color(hex: 0x1C6B41)
    static let amber = Color(hex: 0xE09A26)
    static let red = Color(hex: 0xE05A4F)
    static let muted = Color(hex: 0x5A6B60)
    static let ink = Color(hex: 0xCFE9D8)

    /// Monospaced font used throughout the cockpit.
    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
}

extension Color {
    /// Creates a color from a 24-bit RGB hex literal (e.g. `0x33d17a`).
    init(hex: UInt32) {
        let r = Double((hex >> 16) & 0xFF) / 255
        let g = Double((hex >> 8) & 0xFF) / 255
        let b = Double(hex & 0xFF) / 255
        self.init(.sRGB, red: r, green: g, blue: b, opacity: 1)
    }
}
