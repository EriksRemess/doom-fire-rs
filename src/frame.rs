use std::{
    io::Write,
    time::{Duration, Instant},
};

use crate::{
    AppResult,
    cli::Output,
    fire::{FIRE_PALETTE, Fire},
    kitty,
    terminal::*,
};

pub(crate) struct FrameBuffer {
    bytes: Vec<u8>,
    term_sz: TermSize,
    min_len: u64,
    max_len: u64,
    total_len: u128,
    frame_count: u64,
    start: Instant,
    output: Output,
    kitty_initialized: bool,
    resolution: (usize, usize),
    encoded_len: u64,
    total_written: u128,
    timing: FrameTiming,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct FrameTiming {
    pub(crate) simulation: Duration,
    pub(crate) encoding: Duration,
    pub(crate) write: Duration,
}

impl FrameBuffer {
    #[cfg(test)]
    pub(crate) fn new(
        term_sz: TermSize,
        fg: &[String],
        bg: &[String],
        output: Output,
    ) -> AppResult<Self> {
        let fire = Fire::new(term_sz)?;
        Self::new_for_fire(term_sz, fg, bg, output, &fire)
    }

    pub(crate) fn new_for_fire(
        term_sz: TermSize,
        fg: &[String],
        bg: &[String],
        output: Output,
        fire: &Fire,
    ) -> AppResult<Self> {
        let term_sz = term_sz.validate()?;
        let px_sz = PX.len() + bg[LAST_COLOR].len() + fg[LAST_COLOR].len();
        let screen_sz = px_sz * term_sz.width * (term_sz.height - 1);
        // Cursor positions for every row, initial colors, and the status line.
        let capacity = match output {
            Output::Ansi => screen_sz + term_sz.height * 24 + term_sz.width + 128,
            Output::Kitty => kitty::capacity(fire.pixels.len(), term_sz.width),
        };
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;

        Ok(Self {
            bytes,
            term_sz,
            min_len: 0,
            max_len: 0,
            total_len: 0,
            frame_count: 0,
            start: Instant::now(),
            output,
            kitty_initialized: false,
            resolution: (fire.width, fire.height),
            encoded_len: 0,
            total_written: 0,
            timing: FrameTiming::default(),
        })
    }

    fn draw_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    pub(crate) fn draw_fire(&mut self, fire: &Fire, fg: &[String], bg: &[String]) {
        if self.output == Output::Kitty {
            kitty::draw(&mut self.bytes, fire, self.term_sz, !self.kitty_initialized);
            self.kitty_initialized = true;
            return;
        }
        self.bytes.clear();
        self.draw_str(CURSOR_HOME);
        self.draw_str(COLOR_RESET);
        self.draw_str(&bg[0]);
        self.draw_str(&fg[0]);
        let mut prev_hi = 0;
        let mut prev_lo = 0;

        for y in (0..fire.height).step_by(2) {
            if y != 0 {
                write!(&mut self.bytes, "{CSI}{};1H", y / 2 + 1)
                    .expect("writing into a Vec cannot fail");
            }
            for x in 0..fire.width {
                let hi = fire.pixels[y * fire.width + x];
                let lo = fire.pixels[(y + 1) * fire.width + x];
                if lo != prev_lo {
                    self.draw_str(&bg[FIRE_PALETTE[lo as usize]]);
                }
                if hi != prev_hi {
                    self.draw_str(&fg[FIRE_PALETTE[hi as usize]]);
                }
                self.draw_str(PX);
                prev_hi = hi;
                prev_lo = lo;
            }
        }
    }

    fn record_frame(&mut self, len: u64) {
        if self.frame_count == 0 {
            self.min_len = len;
        }
        self.min_len = self.min_len.min(len);
        self.max_len = self.max_len.max(len);
        self.total_len += u128::from(len);
        self.frame_count += 1;
    }

    fn average_len(&self) -> f64 {
        if self.frame_count == 0 {
            0.0
        } else {
            self.total_len as f64 / self.frame_count as f64
        }
    }

    fn status(&self, elapsed: f64) -> String {
        let fps = if elapsed > 0.0 {
            self.frame_count as f64 / elapsed
        } else {
            0.0
        };
        let ms_per_frame = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
        let mib_per_second = if elapsed > 0.0 {
            self.total_written as f64 / elapsed / 1_048_576.0
        } else {
            0.0
        };
        let mean_ms = |duration: Duration| {
            if self.frame_count == 0 {
                0.0
            } else {
                duration.as_secs_f64() * 1000.0 / self.frame_count as f64
            }
        };
        let mut status = format!(
            "res: {}x{} | {:.2} fps | {:.2} MPix/s | {:.2} MiB/s | {:.2} ms/f | sim/enc/write: {:.2}/{:.2}/{:.2} ms",
            self.resolution.0,
            self.resolution.1,
            fps,
            self.resolution.0 as f64 * self.resolution.1 as f64 * fps / 1_000_000.0,
            mib_per_second,
            ms_per_frame,
            mean_ms(self.timing.simulation),
            mean_ms(self.timing.encoding),
            mean_ms(self.timing.write),
        );
        let (divisor, unit) = binary_unit(self.max_len as f64);
        let sizes = format!(
            " | frame min/avg/max: {:.2}/{:.2}/{:.2} {unit}",
            self.min_len as f64 / divisor,
            self.average_len() / divisor,
            self.max_len as f64 / divisor,
        );
        if status.len() + sizes.len() < self.term_sz.width {
            status.push_str(&sizes);
        }
        status
    }

    fn draw_status(&mut self) {
        let status = self.status(self.start.elapsed().as_secs_f64());
        self.draw_str(&format!("{CSI}{};1H{COLOR_DEF}", self.term_sz.height));
        // The status is ASCII. Leave the bottom-right cell unused to avoid wrapping.
        self.draw_str(&status[..status.len().min(self.term_sz.width - 1)]);
        self.draw_str(LINE_CLEAR_TO_EOL);
    }

    pub(crate) fn size(&self) -> TermSize {
        self.term_sz
    }

    pub(crate) fn exclude_pause(&mut self, paused: Duration) {
        self.start += paused;
        self.kitty_initialized = false;
    }

    pub(crate) fn finish_frame(&mut self) -> &[u8] {
        self.encoded_len = self.bytes.len() as u64;
        self.draw_status();
        &self.bytes
    }

    pub(crate) fn complete_frame(&mut self, timing: FrameTiming) {
        self.record_frame(self.encoded_len);
        self.total_written += self.bytes.len() as u128;
        self.timing.simulation += timing.simulation;
        self.timing.encoding += timing.encoding;
        self.timing.write += timing.write;
    }
}

fn binary_unit(bytes: f64) -> (f64, &'static str) {
    let units = ["B", "KiB", "MiB", "GiB"];
    let mut divisor = 1.0;
    let mut unit_idx = 0;

    while bytes / divisor >= 1024.0 && unit_idx + 1 < units.len() {
        divisor *= 1024.0;
        unit_idx += 1;
    }

    (divisor, units[unit_idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    mod allocations {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;
        use std::mem::DropGuard;

        thread_local! {
            static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
        }

        struct CountingAllocator;

        unsafe impl GlobalAlloc for CountingAllocator {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                let _ = COUNT.try_with(|count| count.set(count.get().map(|n| n + 1)));
                unsafe { System.alloc(layout) }
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                unsafe { System.dealloc(ptr, layout) }
            }
        }

        #[global_allocator]
        static ALLOCATOR: CountingAllocator = CountingAllocator;

        pub fn count(f: impl FnOnce()) -> usize {
            let _reset = DropGuard::new((), |()| COUNT.set(None));
            COUNT.set(Some(0));
            f();
            COUNT.take().unwrap()
        }

        #[test]
        fn counting_resets_after_panic() {
            let result = std::panic::catch_unwind(|| count(|| panic!("test panic")));
            assert!(result.is_err());
            assert_eq!(COUNT.get(), None);
            assert_eq!(count(|| {}), 0);
        }
    }

    #[test]
    fn drawing_fire_does_not_allocate_after_initialization() {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        for (width, height) in [(120, 24), (200, 100)] {
            let size = TermSize { width, height };
            for output in [Output::Ansi, Output::Kitty] {
                let scale = if output == Output::Kitty { 4 } else { 1 };
                let fire = Fire::new_scaled(size, scale).unwrap();
                let mut frame = FrameBuffer::new_for_fire(size, &fg, &bg, output, &fire).unwrap();
                assert_eq!(allocations::count(|| frame.draw_fire(&fire, &fg, &bg)), 0);
                assert_eq!(allocations::count(|| frame.draw_fire(&fire, &fg, &bg)), 0);
            }
        }
    }

    #[test]
    fn formats_binary_bytes() {
        assert_eq!(binary_unit(0.0), (1.0, "B"));
        assert_eq!(binary_unit(1024.0), (1024.0, "KiB"));
    }

    #[test]
    fn benchmark_rates_count_completed_writes_and_use_consistent_units() {
        let size = TermSize {
            width: 100,
            height: 51,
        };
        let mut frame = FrameBuffer::new(
            size,
            &init_colors("38;5;"),
            &init_colors("48;5;"),
            Output::Ansi,
        )
        .unwrap();
        let initial = frame.status(0.0);
        assert!(initial.contains("0.00 fps | 0.00 MPix/s | 0.00 MiB/s | 0.00 ms/f"));
        assert!(!initial.contains("NaN") && !initial.contains("inf"));
        frame.bytes.resize(1_048_576, 0);
        frame.encoded_len = 1_048_000;
        let timing = FrameTiming {
            simulation: Duration::from_millis(5),
            encoding: Duration::from_millis(3),
            write: Duration::from_millis(2),
        };
        frame.complete_frame(timing);
        frame.complete_frame(timing);
        assert!(frame.status(0.1).contains(
            "20.00 fps | 0.20 MPix/s | 20.00 MiB/s | 50.00 ms/f | sim/enc/write: 5.00/3.00/2.00 ms"
        ));
        assert_eq!(frame.average_len(), 1_048_000.0);
        assert_eq!(frame.total_written, 2_097_152);
        let start = frame.start;
        frame.exclude_pause(Duration::from_secs(60));
        assert_eq!(frame.start.duration_since(start), Duration::from_secs(60));
        assert_eq!(frame.frame_count, 2);
    }

    #[test]
    fn frames_keep_fire_and_status_inside_the_terminal() {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        for (width, height) in [(1, 2), (2, 3), (40, 10), (120, 24), (160, 30)] {
            let size = TermSize { width, height };
            let mut fire = Fire::new(size).unwrap();
            assert_eq!(fire.pixels.len(), width * (height - 1) * 2);
            assert!(
                fire.pixels[(fire.height - 1) * width..]
                    .iter()
                    .all(|&px| px == 25)
            );
            let mut rng = Rng::new(1);
            for _ in 0..100 {
                fire.advance(&mut rng);
            }
            assert!(
                fire.pixels
                    .iter()
                    .all(|&px| usize::from(px) < FIRE_PALETTE.len())
            );
            let mut frame = FrameBuffer::new(size, &fg, &bg, Output::Ansi).unwrap();
            frame.draw_fire(&fire, &fg, &bg);
            frame.record_frame(frame.bytes.len() as u64);
            frame.draw_status();
            let output = std::str::from_utf8(&frame.bytes).unwrap();
            assert!(!output.contains(['\r', '\n']));
            // Each row is explicitly positioned; the status has its own final row.
            for row in 1..height {
                let start = format!("{CSI}{row};1H");
                let end = format!("{CSI}{};1H", row + 1);
                let pixels = output
                    .split_once(&start)
                    .unwrap()
                    .1
                    .split_once(&end)
                    .unwrap()
                    .0;
                assert_eq!(pixels.matches(PX).count(), width);
            }
            let status_start = format!("{CSI}{height};1H{COLOR_DEF}");
            let status = output.split_once(&status_start).unwrap().1;
            let text = status.strip_suffix(LINE_CLEAR_TO_EOL).unwrap();
            assert!(text.is_ascii());
            assert!(text.len() < width);
        }
    }

    #[test]
    fn wide_frames_allocate_for_area_and_do_not_grow_while_rendering() {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        let size = TermSize {
            width: 2000,
            height: 22,
        };
        let mut fire = Fire::new(size).unwrap();
        // Alternate colors to exercise the largest encoded frames.
        for (idx, px) in fire.pixels.iter_mut().enumerate() {
            *px = (idx % FIRE_PALETTE.len()) as u8;
        }
        let mut frame = FrameBuffer::new(size, &fg, &bg, Output::Ansi).unwrap();
        let capacity = frame.bytes.capacity();
        assert!(capacity < 2 * 1024 * 1024);
        frame.draw_fire(&fire, &fg, &bg);
        frame.record_frame(frame.bytes.len() as u64);
        frame.draw_status();
        assert_eq!(frame.bytes.capacity(), capacity);
    }

    #[test]
    fn averages_use_the_total_without_accumulated_rounding() {
        let mut frame = FrameBuffer::new(
            TermSize {
                width: 120,
                height: 24,
            },
            &init_colors("38;5;"),
            &init_colors("48;5;"),
            Output::Ansi,
        )
        .unwrap();
        assert_eq!(frame.average_len(), 0.0);
        frame.record_frame(100);
        for _ in 1..1000 {
            frame.record_frame(200);
        }
        assert_eq!(frame.min_len, 100);
        assert_eq!(frame.max_len, 200);
        assert_eq!(format!("{:.2}", frame.average_len()), "199.90");
    }
}
