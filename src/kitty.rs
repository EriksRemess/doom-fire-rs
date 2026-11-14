//! Direct RGB uploads with one reusable image and placement.
//! https://sw.kovidgoyal.net/kitty/graphics-protocol/
use std::io::Write;

use crate::{
    fire::{FIRE_PALETTE, Fire},
    terminal::{CURSOR_HOME, TermSize},
};

const IMAGE_ID: u32 = 0x444f4f4d;
const PIXELS_PER_CHUNK: usize = 1024; // 4096 base64 bytes, three RGB bytes per pixel.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// Uppercase I frees image data as well as placements; affect only our image.
pub(crate) const DELETE_IMAGE: &str = "\x1b_Ga=d,d=I,i=1146048333,q=2\x1b\\";

const fn rgb(index: usize) -> [u8; 3] {
    match index {
        0 => [0, 0, 0],
        3 => [205, 205, 0],
        16..=231 => {
            let levels = [0, 95, 135, 175, 215, 255];
            let color = index - 16;
            [levels[color / 36], levels[color / 6 % 6], levels[color % 6]]
        }
        232..=255 => [8 + 10 * (index as u8 - 232); 3],
        _ => panic!("unsupported fire palette entry"),
    }
}

const fn encode_pixel([r, g, b]: [u8; 3]) -> [u8; 4] {
    [
        BASE64[(r >> 2) as usize],
        BASE64[(((r & 3) << 4) | (g >> 4)) as usize],
        BASE64[(((g & 15) << 2) | (b >> 6)) as usize],
        BASE64[(b & 63) as usize],
    ]
}

const ENCODED_PALETTE: [[u8; 4]; FIRE_PALETTE.len()] = {
    let mut palette = [[0; 4]; FIRE_PALETTE.len()];
    let mut index = 0;
    while index < palette.len() {
        palette[index] = encode_pixel(rgb(FIRE_PALETTE[index]));
        index += 1;
    }
    palette
};

pub(crate) fn capacity(pixels: usize, columns: usize) -> usize {
    pixels * 4 + pixels.div_ceil(PIXELS_PER_CHUNK) * 128 + columns + 256
}

pub(crate) fn draw(bytes: &mut Vec<u8>, fire: &Fire, size: TermSize, create: bool) {
    bytes.clear();
    bytes.extend_from_slice(CURSOR_HOME.as_bytes());
    let chunks = fire.pixels.len().div_ceil(PIXELS_PER_CHUNK);
    for (index, pixels) in fire.pixels.chunks(PIXELS_PER_CHUNK).enumerate() {
        let more = usize::from(index + 1 < chunks);
        if index == 0 {
            if create {
                // Only create on startup, resize, or resume. Re-transmitting an
                // image each frame deletes its placement mid-upload in Kitty.
                write!(
                    bytes,
                    "\x1b_Ga=T,t=d,f=24,s={},v={},i={IMAGE_ID},p=1,c={},r={},C=1,q=2,m={more};",
                    fire.width,
                    fire.height,
                    size.width,
                    size.height - 1
                )
                .expect("writing into a Vec cannot fail");
            } else {
                write!(
                    bytes,
                    "\x1b_Ga=f,f=24,s={},v={},i={IMAGE_ID},r=1,X=1,q=2,m={more};",
                    fire.width, fire.height
                )
                .expect("writing into a Vec cannot fail");
            }
        } else if !create {
            // Kitty 0.48 reads these from the final chunk as well.
            write!(bytes, "\x1b_Ga=f,r=1,X=1,m={more},q=2;")
                .expect("writing into a Vec cannot fail");
        } else {
            write!(bytes, "\x1b_Gm={more},q=2;").expect("writing into a Vec cannot fail");
        }
        for &pixel in pixels {
            bytes.extend_from_slice(&ENCODED_PALETTE[pixel as usize]);
        }
        bytes.extend_from_slice(b"\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_rgb_palette_with_standard_base64() {
        assert_eq!(encode_pixel(*b"abc"), *b"YWJj");
        assert_eq!(encode_pixel([255; 3]), *b"////");
        assert_eq!(rgb(52), [95, 0, 0]);
        assert_eq!(rgb(233), [18; 3]);
        assert_eq!(rgb(230), [255, 255, 215]);
        assert!(DELETE_IMAGE.contains(&format!("i={IMAGE_ID},")));
    }

    #[test]
    fn uploads_complete_chunked_frames_without_growing_buffer() {
        for (width, height) in [(1, 2), (512, 2), (513, 2), (120, 24), (2000, 22)] {
            let size = TermSize { width, height };
            let mut fire = Fire::new(size).unwrap();
            for (index, pixel) in fire.pixels.iter_mut().enumerate() {
                *pixel = (index % FIRE_PALETTE.len()) as u8;
            }
            let mut bytes = Vec::with_capacity(capacity(fire.pixels.len(), size.width));
            let capacity = bytes.capacity();
            draw(&mut bytes, &fire, size, true);
            assert_eq!(bytes.capacity(), capacity);
            let text = std::str::from_utf8(&bytes).unwrap();
            let commands: Vec<_> = text
                .strip_prefix(CURSOR_HOME)
                .unwrap()
                .split("\x1b\\")
                .filter(|chunk| !chunk.is_empty())
                .collect();
            let mut payload = Vec::new();
            for (index, command) in commands.iter().enumerate() {
                let (header, chunk) = command
                    .strip_prefix("\x1b_G")
                    .unwrap()
                    .split_once(';')
                    .unwrap();
                assert!(header.contains("q=2"));
                assert!(header.contains(if index + 1 == commands.len() {
                    "m=0"
                } else {
                    "m=1"
                }));
                assert!(chunk.len() <= 4096 && chunk.len() % 4 == 0);
                if index == 0 {
                    assert!(header.contains(&format!("s={width},v={}", (height - 1) * 2)));
                    assert!(header.contains(&format!("c={width},r={},C=1", height - 1)));
                }
                payload.extend_from_slice(chunk.as_bytes());
            }
            assert_eq!(payload.len(), fire.pixels.len() * 4);
            for (index, chunk) in payload.as_chunks::<4>().0.iter().enumerate() {
                let values: Vec<_> = chunk
                    .iter()
                    .map(|byte| BASE64.iter().position(|x| x == byte).unwrap() as u8)
                    .collect();
                let decoded = [
                    values[0] << 2 | values[1] >> 4,
                    values[1] << 4 | values[2] >> 2,
                    values[2] << 6 | values[3],
                ];
                assert_eq!(decoded, rgb(FIRE_PALETTE[index % FIRE_PALETTE.len()]));
            }
        }
    }

    #[test]
    fn frame_updates_preserve_edit_target_on_every_chunk() {
        let size = TermSize {
            width: 120,
            height: 24,
        };
        let fire = Fire::new_scaled(size, 4).unwrap();
        let mut bytes = Vec::with_capacity(capacity(fire.pixels.len(), size.width));
        draw(&mut bytes, &fire, size, false);
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut chunks = 0;
        for command in text
            .strip_prefix(CURSOR_HOME)
            .unwrap()
            .split("\x1b\\")
            .filter(|s| !s.is_empty())
        {
            let (header, payload) = command.split_once(';').unwrap();
            assert!(header.starts_with("\x1b_Ga=f,"));
            assert!(header.contains("r=1,X=1,"));
            assert!(header.contains("q=2"));
            assert!(payload.len() <= 4096);
            chunks += 1;
        }
        assert!(chunks > 1);
        assert!(!text.contains("a=T"));
    }
}
