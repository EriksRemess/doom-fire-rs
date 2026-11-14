use std::{
    process,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    pub(crate) fn seeded() -> Self {
        Self::new(random_seed())
    }

    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(crate) fn one_in(&mut self, denominator: usize) -> bool {
        self.next_u64().is_multiple_of(denominator as u64)
    }

    pub(crate) fn next_0_to_3(&mut self) -> usize {
        (self.next_u64() & 3) as usize
    }
}

fn random_seed() -> u64 {
    #[cfg(unix)]
    {
        use std::io::Read;

        let mut bytes = [0_u8; 8];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .is_ok()
        {
            let seed = u64::from_ne_bytes(bytes);
            if seed != 0 {
                return seed;
            }
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    now ^ (process::id() as u64).rotate_left(17) ^ 0xA5A5_1F1F_D00D_F00D
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_range_is_bounded() {
        let mut rng = Rng { state: 1 };
        for _ in 0..1_000 {
            assert!(rng.next_0_to_3() <= 3);
        }
    }
}
