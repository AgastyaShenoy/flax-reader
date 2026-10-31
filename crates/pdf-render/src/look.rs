/// How page content is colored. Applied to each tile once, on the render thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Look {
    #[default]
    Normal,
    /// White becomes dark gray and black light gray. Lightness flips but hue is kept, so red text
    /// stays red and links stay blue.
    Dark,
    /// Ink on warm paper.
    Sepia,
    /// Like Dark, but white becomes pure black, which OLED screens show by turning pixels off.
    Black,
}

const DARK_PAPER: i32 = 0x26;
const DARK_INK: i32 = 0xDC;
const BLACK_INK: i32 = 0xD0;
const SEPIA_PAPER: [u32; 3] = [247, 238, 214];
const SEPIA_INK: [u32; 3] = [44, 36, 28];

impl Look {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Look::Dark,
            2 => Look::Sepia,
            3 => Look::Black,
            _ => Look::Normal,
        }
    }

    /// What white paper turns into: the color of a page whose tiles haven't rendered yet.
    pub fn paper(self) -> u32 {
        let mut px = [0x00FF_FFFF];
        self.apply(&mut px);
        px[0]
    }

    /// Recolors 0x00RRGGBB pixels in place.
    pub fn apply(self, pixels: &mut [u32]) {
        match self {
            Look::Normal => {}
            Look::Dark => flip(pixels, DARK_PAPER, DARK_INK),
            Look::Black => flip(pixels, 0, BLACK_INK),
            Look::Sepia => {
                for p in pixels {
                    let [_, r, g, b] = p.to_be_bytes().map(u32::from);
                    let mix = |c: u32, i: usize| (SEPIA_INK[i] + c * (SEPIA_PAPER[i] - SEPIA_INK[i]) / 255) as u8;
                    *p = u32::from_be_bytes([0, mix(r, 0), mix(g, 1), mix(b, 2)]);
                }
            }
        }
    }
}

/// Maps lightness so white becomes `paper` and black `ink`, shifting each channel alike so hue is kept.
fn flip(pixels: &mut [u32], paper: i32, ink: i32) {
    for p in pixels {
        let [_, r, g, b] = p.to_be_bytes().map(i32::from);
        let luma = (r * 77 + g * 150 + b * 29) >> 8;
        let target = paper + (255 - luma) * (ink - paper) / 255;
        let shift = |c: i32| (c + target - luma).clamp(0, 255) as u8;
        *p = u32::from_be_bytes([0, shift(r), shift(g), shift(b)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_flips_lightness_and_keeps_hue() {
        let mut px = [0x00FF_FFFF, 0x0000_0000, 0x00FF_0000];
        Look::Dark.apply(&mut px);
        assert_eq!(px[0], 0x0026_2626);
        assert_eq!(px[1], 0x00DC_DCDC);
        let [_, r, g, b] = px[2].to_be_bytes();
        assert!(r > g && r > b, "red stays red: {:06x}", px[2]);
    }

    #[test]
    fn black_paper_is_pure_black() {
        let mut px = [0x00FF_FFFF, 0x0000_0000, 0x0000_00FF];
        Look::Black.apply(&mut px);
        assert_eq!(px[0], 0);
        assert_eq!(px[1], 0x00D0_D0D0);
        let [_, r, g, b] = px[2].to_be_bytes();
        assert!(b > r && b > g, "blue stays blue: {:06x}", px[2]);
    }

    #[test]
    fn sepia_paper_and_ink() {
        let mut px = [0x00FF_FFFF, 0];
        Look::Sepia.apply(&mut px);
        assert_eq!(px, [0x00F7_EED6, 0x002C_241C]);
    }
}
