use crate::{AppResult, rng::Rng, terminal::TermSize};

pub(crate) const FIRE_PALETTE: [usize; 26] = [
    0, 233, 234, 52, 53, 88, 89, 94, 95, 96, 130, 131, 132, 133, 172, 214, 215, 220, 220, 221, 3,
    226, 227, 230, 195, 230,
];

pub(crate) struct Fire {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) pixels: Vec<u8>,
    cooling_scale: usize,
}

impl Fire {
    pub(crate) fn new(size: TermSize) -> AppResult<Self> {
        Self::new_scaled(size, 1)
    }

    pub(crate) fn new_scaled(size: TermSize, scale: usize) -> AppResult<Self> {
        let size = size.validate()?;
        if !(1..=16).contains(&scale) {
            return Err("fire scale must be between 1 and 16".into());
        }
        // Keep the final terminal row available for statistics.
        let height = (size.height - 1) * 2 * scale;
        let width = size.width * scale;
        let cells = width
            .checked_mul(height)
            .filter(|&n| n <= 16_777_216)
            .ok_or("fire image exceeds 16,777,216 pixels; reduce --kitty-scale or terminal size")?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(cells)?;
        pixels.resize(cells, 0);
        pixels[(height - 1) * width..].fill((FIRE_PALETTE.len() - 1) as u8);
        Ok(Self {
            width,
            height,
            pixels,
            cooling_scale: scale,
        })
    }

    pub(crate) fn advance(&mut self, rng: &mut Rng) {
        for x in 0..self.width {
            for y in 0..self.height {
                let idx = y * self.width + x;
                let px = self.pixels[idx];
                if px == 0 && idx >= self.width {
                    self.pixels[idx - self.width] = 0;
                } else {
                    let spread = rng.next_0_to_3();
                    let dst = if idx > spread { idx - spread + 1 } else { idx };
                    if dst >= self.width {
                        // Preserve the approximate flame height as the simulation is
                        // refined: cooling happens 1/scale as often per pixel.
                        let cool = self.cooling_scale == 1 || rng.one_in(self.cooling_scale);
                        self.pixels[dst - self.width] =
                            px.saturating_sub((spread & 1) as u8 * u8::from(cool));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::MAX_COLOR;

    #[test]
    fn scaled_fire_is_bounded_and_keeps_a_hot_source() {
        let size = TermSize {
            width: 120,
            height: 24,
        };
        let mut fire = Fire::new_scaled(size, 4).unwrap();
        assert_eq!((fire.width, fire.height), (480, 184));
        let mut rng = Rng::new(1);
        for _ in 0..200 {
            fire.advance(&mut rng);
        }
        assert!(
            fire.pixels
                .iter()
                .all(|&p| usize::from(p) < FIRE_PALETTE.len())
        );
        assert!(
            fire.pixels[(fire.height - 1) * fire.width..]
                .iter()
                .all(|&p| p == 25)
        );
        assert!(Fire::new_scaled(size, 0).is_err());
        assert!(
            Fire::new_scaled(
                TermSize {
                    width: 1000,
                    height: 1000
                },
                16
            )
            .is_err()
        );
    }

    #[test]
    fn fire_palette_indexes_fit_color_table() {
        assert!(FIRE_PALETTE.iter().all(|idx| *idx < MAX_COLOR));
    }

    #[test]
    fn rejects_invalid_and_excessive_dimensions() {
        for (width, height) in [
            (0, 24),
            (120, 0),
            (120, 1),
            (usize::MAX, 22),
            (120, usize::MAX),
            (1001, 1000),
        ] {
            assert!(TermSize { width, height }.validate().is_err());
            assert!(Fire::new(TermSize { width, height }).is_err());
        }
        assert!(
            TermSize {
                width: 1000,
                height: 1000
            }
            .validate()
            .is_ok()
        );
    }
}
