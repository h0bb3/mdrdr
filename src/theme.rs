//! Colors, sizes, margins. Everything tweakable lives here.

pub type Rgba = [u8; 4];

#[derive(Clone)]
pub struct Theme {
    pub bg: Rgba,
    pub fg: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub code_bg: Rgba,

    pub body_size: f32,
    pub heading_size: f32,
    pub mono_size: f32,

    pub margin_x: f32,
    pub margin_y: f32,
    pub line_height_mult: f32,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            bg: [0xfb, 0xf9, 0xf4, 0xff],       // warm paper
            fg: [0x24, 0x28, 0x2c, 0xff],       // near-black
            muted: [0x60, 0x66, 0x6d, 0xff],
            accent: [0x1d, 0x6f, 0x42, 0xff],   // srcful-ish green
            code_bg: [0xef, 0xec, 0xe3, 0xff],

            body_size: 18.0,
            heading_size: 32.0,
            mono_size: 16.0,

            margin_x: 48.0,
            margin_y: 48.0,
            line_height_mult: 1.45,
        }
    }
}
