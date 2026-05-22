//! Font loading. Bundles DejaVu via include_bytes so the binary is self-contained.

use fontdue::{Font, FontSettings};

pub struct Fonts {
    pub body: Font,
    pub bold: Font,
    pub italic: Font,
    pub bold_italic: Font,
    pub mono: Font,
    /// Monochrome emoji font (OpenMoji-black-glyf). Covers Emoticons, Misc
    /// Symbols and Pictographs, Supplemental Symbols, Transport, etc.
    pub emoji: Font,
}

impl Fonts {
    pub fn load() -> Self {
        let body = include_bytes!("../assets/fonts/DejaVuSans-Regular.ttf");
        let bold = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");
        let italic = include_bytes!("../assets/fonts/DejaVuSans-Italic.ttf");
        let bold_italic = include_bytes!("../assets/fonts/DejaVuSans-BoldItalic.ttf");
        let mono = include_bytes!("../assets/fonts/DejaVuSansMono-Regular.ttf");
        let emoji = include_bytes!("../assets/fonts/OpenMoji-black-glyf.ttf");
        let s = FontSettings::default();
        Self {
            body: Font::from_bytes(body.as_slice(), s).unwrap(),
            bold: Font::from_bytes(bold.as_slice(), s).unwrap(),
            italic: Font::from_bytes(italic.as_slice(), s).unwrap(),
            bold_italic: Font::from_bytes(bold_italic.as_slice(), s).unwrap(),
            mono: Font::from_bytes(mono.as_slice(), s).unwrap(),
            emoji: Font::from_bytes(emoji.as_slice(), s).unwrap(),
        }
    }
}

/// True for codepoints we want to route through the emoji font. Covers the
/// common "user-thinks-of-it-as-emoji" ranges: Emoticons, Misc Symbols
/// and Pictographs, Transport and Map, Supplemental Symbols, and a few
/// smaller ranges. Non-exhaustive — extend as we hit gaps.
pub fn is_emoji(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x1F300..=0x1F64F   // Misc Symbols/Pictographs, Emoticons
        | 0x1F680..=0x1F6FF // Transport and Map
        | 0x1F700..=0x1F77F // Alchemical
        | 0x1F780..=0x1F7FF // Geometric Shapes Extended
        | 0x1F800..=0x1F8FF // Supplemental Arrows-C
        | 0x1F900..=0x1F9FF // Supplemental Symbols and Pictographs
        | 0x1FA00..=0x1FA6F // Chess / Symbols and Pictographs Extended-A
        | 0x1FA70..=0x1FAFF // Symbols and Pictographs Extended-A
        | 0x2600..=0x26FF   // Misc Symbols
        | 0x2700..=0x27BF   // Dingbats
    )
}

/// Shape for a "pure colour" emoji that we draw as a flat primitive instead
/// of routing through fontdue (which only gives us a monochrome silhouette,
/// collapsing 🟢/🟡/🔴 into the same gray blob).
pub enum ColorEmojiShape {
    Circle,
    Square,
}

/// If `ch` is a colour-coded geometric emoji (large/medium circle or square
/// in any of the standard hues), return its shape and fill colour. Returning
/// `None` means "render normally via the emoji font".
pub fn color_emoji(ch: char) -> Option<(ColorEmojiShape, [u8; 4])> {
    use ColorEmojiShape::*;
    let (shape, rgb) = match ch as u32 {
        // Circles
        0x1F534 => (Circle, [0xE7, 0x4C, 0x3C]), // 🔴
        0x1F7E0 => (Circle, [0xE6, 0x7E, 0x22]), // 🟠
        0x1F7E1 => (Circle, [0xF1, 0xC4, 0x0F]), // 🟡
        0x1F7E2 => (Circle, [0x27, 0xAE, 0x60]), // 🟢
        0x1F535 => (Circle, [0x29, 0x80, 0xB9]), // 🔵
        0x1F7E3 => (Circle, [0x8E, 0x44, 0xAD]), // 🟣
        0x1F7E4 => (Circle, [0x96, 0x5A, 0x3E]), // 🟤
        0x26AB => (Circle, [0x33, 0x33, 0x33]),  // ⚫
        0x26AA => (Circle, [0xEC, 0xEC, 0xEC]),  // ⚪
        // Squares
        0x1F7E5 => (Square, [0xE7, 0x4C, 0x3C]), // 🟥
        0x1F7E7 => (Square, [0xE6, 0x7E, 0x22]), // 🟧
        0x1F7E8 => (Square, [0xF1, 0xC4, 0x0F]), // 🟨
        0x1F7E9 => (Square, [0x27, 0xAE, 0x60]), // 🟩
        0x1F7E6 => (Square, [0x29, 0x80, 0xB9]), // 🟦
        0x1F7EA => (Square, [0x8E, 0x44, 0xAD]), // 🟪
        0x1F7EB => (Square, [0x96, 0x5A, 0x3E]), // 🟫
        0x2B1B => (Square, [0x33, 0x33, 0x33]),  // ⬛
        0x2B1C => (Square, [0xEC, 0xEC, 0xEC]),  // ⬜
        _ => return None,
    };
    Some((shape, [rgb[0], rgb[1], rgb[2], 0xFF]))
}
