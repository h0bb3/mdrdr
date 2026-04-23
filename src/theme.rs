//! Colors, sizes, margins. Everything tweakable lives here.

pub type Rgba = [u8; 4];

#[derive(Clone)]
pub struct Theme {
    pub bg: Rgba,
    pub fg: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub code_bg: Rgba,
    pub sidebar_bg: Rgba,
    pub sidebar_active_bg: Rgba,

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
            sidebar_bg: [0xf3, 0xef, 0xe5, 0xff],
            sidebar_active_bg: [0xe3, 0xdc, 0xc9, 0xff],

            body_size: 18.0,
            heading_size: 32.0,
            mono_size: 16.0,

            margin_x: 48.0,
            margin_y: 48.0,
            line_height_mult: 1.45,
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: [0x1e, 0x22, 0x28, 0xff],       // near-black
            fg: [0xd7, 0xdc, 0xe3, 0xff],       // off-white
            muted: [0x8a, 0x93, 0x9d, 0xff],
            accent: [0x7c, 0xc9, 0x8e, 0xff],   // lighter green
            code_bg: [0x2a, 0x30, 0x38, 0xff],
            sidebar_bg: [0x25, 0x2a, 0x31, 0xff],
            sidebar_active_bg: [0x3a, 0x43, 0x4d, 0xff],

            body_size: 18.0,
            heading_size: 32.0,
            mono_size: 16.0,

            margin_x: 48.0,
            margin_y: 48.0,
            line_height_mult: 1.45,
        }
    }
}
