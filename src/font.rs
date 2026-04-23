//! Font loading. Bundles DejaVu via include_bytes so the binary is self-contained.

use fontdue::{Font, FontSettings};

pub struct Fonts {
    pub body: Font,
    pub bold: Font,
    pub italic: Font,
    pub mono: Font,
}

impl Fonts {
    pub fn load() -> Self {
        let body = include_bytes!("../assets/fonts/DejaVuSans-Regular.ttf");
        let bold = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");
        let italic = include_bytes!("../assets/fonts/DejaVuSans-Italic.ttf");
        let mono = include_bytes!("../assets/fonts/DejaVuSansMono-Regular.ttf");
        let s = FontSettings::default();
        Self {
            body: Font::from_bytes(body.as_slice(), s).unwrap(),
            bold: Font::from_bytes(bold.as_slice(), s).unwrap(),
            italic: Font::from_bytes(italic.as_slice(), s).unwrap(),
            mono: Font::from_bytes(mono.as_slice(), s).unwrap(),
        }
    }
}
