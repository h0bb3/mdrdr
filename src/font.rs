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
