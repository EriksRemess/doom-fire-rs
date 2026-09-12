use std::{
    fmt,
    io::{self, BufRead, IsTerminal, Write},
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_COLOR: usize = 256;
const LAST_COLOR: usize = MAX_COLOR - 1;
// Bound both the simulation and the worst-case encoded frame to a few dozen MiB.
const MAX_TERM_CELLS: usize = 1_000_000;

const CSI: &str = "\x1B[";
const LINE_CLEAR_TO_EOL: &str = "\x1B[0K";
const LINE_NEW: &str = "\x1B[0K\x1B[1E\x1B[1G";
const CURSOR_SAVE: &str = "\x1B7";
const CURSOR_LOAD: &str = "\x1B8";
const CURSOR_SHOW: &str = "\x1B[?25h";
const CURSOR_HIDE: &str = "\x1B[?25l";
const CURSOR_HOME: &str = "\x1B[1;1H";
const SCREEN_CLEAR: &str = "\x1B[2J";
const SCREEN_BUF_ON: &str = "\x1B[?1049h";
const SCREEN_BUF_OFF: &str = "\x1B[?1049l";
const CHAR_SET_ASCII: &str = "\x1B(B";
const COLOR_RESET: &str = "\x1B[0m";
const COLOR_DEF: &str = "\x1B[48;5;0m\x1B[38;5;15m";
const COLOR_ITALIC: &str = "\x1B[3m";
const COLOR_NOT_ITALIC: &str = "\x1B[23m";
const PX: &str = "\u{2580}";

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

const FIRE_PALETTE: [usize; 26] = [
    0, 233, 234, 52, 53, 88, 89, 94, 95, 96, 130, 131, 132, 133, 172, 214, 215, 220, 220, 221, 3,
    226, 227, 230, 195, 230,
];

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TermSize {
    height: usize,
    width: usize,
}

impl TermSize {
    fn validate(self) -> io::Result<Self> {
        if self.width == 0 || self.height < 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal must be at least 1 column by 2 rows",
            ));
        }
        if self
            .width
            .checked_mul(self.height)
            .is_none_or(|cells| cells > MAX_TERM_CELLS)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal exceeds the limit of 1,000,000 cells",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy)]
enum PauseScreen {
    SizeWarning,
    Capabilities,
}

struct App {
    stdout: io::Stdout,
    console: platform::Console,
    term_sz: TermSize,
    fg: Vec<String>,
    bg: Vec<String>,
    rng: Rng,
    terminal_active: bool,
}

impl App {
    fn new() -> AppResult<Self> {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        let console = platform::init_console()?;
        let term_sz = platform::term_size(&console)?.validate()?;
        let rng = Rng::seeded();

        let mut app = Self {
            stdout: io::stdout(),
            console,
            term_sz,
            fg,
            bg,
            rng,
            terminal_active: true,
        };

        app.emit(&term_on())?;
        if cfg!(windows) {
            app.emit(CHAR_SET_ASCII)?;
        }

        Ok(app)
    }

    fn run(&mut self) -> AppResult<()> {
        self.check_term_size()?;
        if interrupted() {
            return Ok(());
        }

        self.term_sz = platform::term_size(&self.console)?.validate()?;
        self.show_term_capabilities()?;
        if interrupted() {
            return Ok(());
        }

        self.show_doom_fire()
    }

    fn complete(&mut self) -> io::Result<()> {
        if self.terminal_active {
            #[cfg(unix)]
            platform::resume_output();
            self.emit(&term_off())?;
            self.stdout.flush()?;
            self.terminal_active = false;
        }
        Ok(())
    }

    fn emit(&mut self, s: &str) -> io::Result<()> {
        self.emit_bytes(s.as_bytes())
    }

    fn emit_fmt(&mut self, args: fmt::Arguments<'_>) -> io::Result<()> {
        self.emit(&args.to_string())
    }

    fn emit_fg(&mut self, idx: usize) -> io::Result<()> {
        let bytes = self.fg[idx].as_bytes().to_vec();
        self.emit_bytes(&bytes)
    }

    fn emit_bg(&mut self, idx: usize) -> io::Result<()> {
        let bytes = self.bg[idx].as_bytes().to_vec();
        self.emit_bytes(&bytes)
    }

    fn emit_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(windows)]
        {
            platform::write_console(&self.console, bytes)
        }

        #[cfg(not(windows))]
        self.stdout.write_all(bytes)
    }

    fn pause(&mut self, screen: PauseScreen) -> AppResult<()> {
        self.emit(COLOR_RESET)?;
        self.emit("Press return to continue...")?;
        self.stdout.flush()?;

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = read_prompt(&mut io::stdin().lock());
            let _ = sender.send(result);
        });

        while !interrupted() {
            if self.handle_suspend()?.is_some() && !interrupted() {
                match screen {
                    PauseScreen::SizeWarning => self.show_size_warning()?,
                    PauseScreen::Capabilities => self.show_capability_colors()?,
                }
                self.emit(COLOR_RESET)?;
                self.emit("Press return to continue...")?;
                self.stdout.flush()?;
            }
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(true)) => {
                    INTERRUPTED.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(Ok(_)) => break,
                Ok(Err(err)) if err.kind() == io::ErrorKind::Interrupted && interrupted() => break,
                Ok(Err(err)) => return Err(err.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        Ok(())
    }

    fn check_term_size(&mut self) -> AppResult<()> {
        let min_w = 120;
        let min_h = 22;
        let width = self.term_sz.width;
        let height = self.term_sz.height;
        let w_ok = width >= min_w;
        let h_ok = height >= min_h;

        if w_ok && h_ok {
            return Ok(());
        }

        self.show_size_warning()?;
        self.pause(PauseScreen::SizeWarning)?;
        if !interrupted() {
            self.emit(COLOR_RESET)?;
            self.emit(CURSOR_HOME)?;
            self.emit(SCREEN_CLEAR)?;
        }
        Ok(())
    }

    fn show_size_warning(&mut self) -> AppResult<()> {
        let min_w = 120;
        let min_h = 22;
        let width = self.term_sz.width;
        let height = self.term_sz.height;
        let w_ok = width >= min_w;
        let h_ok = height >= min_h;
        if w_ok && h_ok {
            return self.show_term_size();
        }
        self.emit_fg(9)?;

        if w_ok && !h_ok {
            self.emit_fmt(format_args!(
                "Screen may be too short - height is {} and need {}.",
                height, min_h
            ))?;
        } else if !w_ok && h_ok {
            self.emit_fmt(format_args!(
                "Screen may be too narrow - width is {} and need {}.",
                width, min_w
            ))?;
        } else {
            self.emit_fmt(format_args!(
                "Screen is too small - have {} x {} and need {} x {}",
                width, height, min_w, min_h
            ))?;
        }

        self.emit(nl())?;
        self.emit(nl())?;
        self.emit_bg(1)?;
        self.emit_fg(15)?;
        self.emit("There may be rendering issues on the next screen; to correct, <q><enter>, resize and try again.")?;
        self.emit(LINE_CLEAR_TO_EOL)?;
        self.emit(COLOR_RESET)?;
        self.emit(nl())?;
        self.emit(nl())?;
        self.emit("Continue?")?;
        self.emit(nl())?;
        self.emit(nl())?;

        Ok(())
    }

    fn show_term_size(&mut self) -> AppResult<()> {
        let width = self.term_sz.width;
        let height = self.term_sz.height;

        self.emit(COLOR_DEF)?;
        self.emit_fmt(format_args!("Screen size: {width}w x {height}h"))?;
        self.emit(nl())?;
        self.emit(nl())?;
        Ok(())
    }

    fn show_label(&mut self, label: &str) -> AppResult<()> {
        self.emit(COLOR_DEF)?;
        self.emit_fmt(format_args!("{COLOR_DEF}{label}:"))?;
        self.emit(nl())?;
        Ok(())
    }

    fn show_standard_colors(&mut self) -> AppResult<()> {
        self.show_label("Standard colors")?;
        self.emit_fg(15)?;

        for color_idx in 0..8 {
            self.emit_bg(color_idx)?;
            if color_idx == 7 {
                self.emit_fg(0)?;
            }
            self.emit_fmt(format_args!("{} {:2}  ", sep(), color_idx))?;
        }

        self.emit(COLOR_DEF)?;
        self.emit(nl())?;

        self.emit_fg(15)?;
        for color_idx in 8..16 {
            self.emit_bg(color_idx)?;
            if color_idx == 15 {
                self.emit_fg(0)?;
            }
            self.emit_fmt(format_args!("{} {:2}  ", sep(), color_idx))?;
        }

        self.emit(COLOR_DEF)?;
        self.emit(nl())?;
        self.emit(nl())?;
        Ok(())
    }

    fn show_216_colors(&mut self) -> AppResult<()> {
        self.show_label("216 colors")?;

        for color_shift in 0..6 {
            let color_addendum = color_shift * 36 + 16;

            for color_idx in 0..36 {
                let bg_idx = color_idx + color_addendum;
                let fg_idx = if color_idx > 17 { 0 } else { 15 };

                self.emit_bg(bg_idx)?;
                self.emit_fg(fg_idx)?;
                self.emit_fmt(format_args!("{bg_idx:3}"))?;
            }

            self.emit(COLOR_DEF)?;
            self.emit(nl())?;
        }

        self.emit(COLOR_DEF)?;
        self.emit(nl())?;
        Ok(())
    }

    fn show_grayscale(&mut self) -> AppResult<()> {
        self.show_label("Grayscale")?;
        self.emit_fg(15)?;

        for bg_idx in 232..256 {
            if bg_idx > 243 {
                self.emit_fg(0)?;
            }

            self.emit_bg(bg_idx)?;
            self.emit_fmt(format_args!("{}{bg_idx} ", sep()))?;
        }

        self.emit(COLOR_DEF)?;
        self.emit(nl())?;
        self.emit(COLOR_DEF)?;
        self.emit(nl())?;
        Ok(())
    }

    fn prepare_marquee(&mut self) -> AppResult<()> {
        self.emit(CURSOR_SAVE)?;
        self.emit_bg(222)?;
        for _ in 0..4 {
            self.emit(LINE_CLEAR_TO_EOL)?;
            self.emit(nl())?;
        }
        Ok(())
    }

    fn marquee_sleep(&mut self, duration: Duration) -> AppResult<()> {
        let deadline = Instant::now() + duration;
        while !interrupted() {
            if self.handle_suspend()?.is_some() {
                if !interrupted() {
                    self.show_capability_colors()?;
                    self.prepare_marquee()?;
                }
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(remaining.min(Duration::from_millis(25)));
        }
        Ok(())
    }

    fn scroll_marquee(&mut self) -> AppResult<()> {
        let bg_idx = 222;
        self.prepare_marquee()?;
        let text = [
            format!(
                "  Things move along so rapidly nowadays that people saying {COLOR_ITALIC}It can't be done{COLOR_NOT_ITALIC} are always being interrupted"
            ),
            format!(
                "  by somebody doing it.                                                                    {COLOR_ITALIC}-- Puck, 1902{COLOR_NOT_ITALIC}"
            ),
            "  Test your might!".to_string(),
            format!("  {COLOR_ITALIC}-- Mortal Kombat{COLOR_NOT_ITALIC}"),
            "  How much is the fish?".to_string(),
            format!("             {COLOR_ITALIC}-- Scooter{COLOR_NOT_ITALIC}"),
        ];

        let fade_seq = [222, 221, 220, 215, 214, 184, 178, 130, 235, 58, 16];

        for txt_idx in 0..(text.len() / 2) {
            for fade in fade_seq {
                if interrupted() {
                    return Ok(());
                }

                self.emit(CURSOR_LOAD)?;
                self.emit_bg(bg_idx)?;
                self.emit(nl())?;

                self.emit_fg(fade)?;
                self.emit(&text[txt_idx * 2])?;
                self.emit(LINE_CLEAR_TO_EOL)?;
                self.emit(nl())?;
                self.emit(&text[txt_idx * 2 + 1])?;
                self.emit(LINE_CLEAR_TO_EOL)?;
                self.emit(nl())?;

                self.marquee_sleep(Duration::from_millis(10))?;
            }

            self.marquee_sleep(Duration::from_millis(1_000))?;

            for fade in fade_seq[1..].iter().rev().copied() {
                if interrupted() {
                    return Ok(());
                }

                self.emit(CURSOR_LOAD)?;
                self.emit_bg(bg_idx)?;
                self.emit(nl())?;

                self.emit_fg(fade)?;
                self.emit(&text[txt_idx * 2])?;
                self.emit(LINE_CLEAR_TO_EOL)?;
                self.emit(nl())?;
                self.emit(&text[txt_idx * 2 + 1])?;
                self.emit(LINE_CLEAR_TO_EOL)?;
                self.emit(nl())?;

                self.marquee_sleep(Duration::from_millis(10))?;
            }

            self.emit(nl())?;
        }

        Ok(())
    }

    fn show_capability_colors(&mut self) -> AppResult<()> {
        self.show_term_size()?;
        self.show_standard_colors()?;
        self.show_216_colors()?;
        self.show_grayscale()
    }

    fn show_term_capabilities(&mut self) -> AppResult<()> {
        self.show_capability_colors()?;
        self.scroll_marquee()?;
        if interrupted() {
            Ok(())
        } else {
            self.pause(PauseScreen::Capabilities)
        }
    }

    fn handle_suspend(&mut self) -> AppResult<Option<Duration>> {
        #[cfg(unix)]
        if !interrupted() && platform::take_suspend_request() {
            let start = Instant::now();
            self.complete()?;
            platform::suspend_process()?;
            if !interrupted() {
                self.term_sz = platform::term_size(&self.console)?.validate()?;
                self.terminal_active = true;
                self.emit(&term_on())?;
                self.stdout.flush()?;
            }
            return Ok(Some(start.elapsed()));
        }
        Ok(None)
    }

    fn show_doom_fire(&mut self) -> AppResult<()> {
        self.term_sz = platform::term_size(&self.console)?.validate()?;
        let mut fire = Fire::new(self.term_sz)?;
        let mut frame = FrameBuffer::new(self.term_sz, &self.fg, &self.bg)?;
        self.emit(SCREEN_CLEAR)?;

        while !interrupted() {
            if let Some(paused) = self.handle_suspend()? {
                frame.start += paused;
            }
            if interrupted() {
                break;
            }
            let size = platform::term_size(&self.console)?.validate()?;
            if size != frame.term_sz {
                fire = Fire::new(size)?;
                frame = FrameBuffer::new(size, &self.fg, &self.bg)?;
                self.term_sz = size;
                self.emit(SCREEN_CLEAR)?;
            }

            fire.advance(&mut self.rng);
            frame.draw_fire(&fire, &self.fg, &self.bg);
            frame.paint(self)?;
        }

        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Also restore the terminal on early returns and unwinding.
        let _ = self.complete();
    }
}

struct Fire {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Fire {
    fn new(size: TermSize) -> AppResult<Self> {
        let size = size.validate()?;
        // Keep the final terminal row available for statistics.
        let height = (size.height - 1) * 2;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(height * size.width)?;
        pixels.resize(height * size.width, 0);
        pixels[(height - 1) * size.width..].fill((FIRE_PALETTE.len() - 1) as u8);
        Ok(Self {
            width: size.width,
            height,
            pixels,
        })
    }

    fn advance(&mut self, rng: &mut Rng) {
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
                        self.pixels[dst - self.width] = px.saturating_sub((spread & 1) as u8);
                    }
                }
            }
        }
    }
}

struct FrameBuffer {
    bytes: Vec<u8>,
    term_sz: TermSize,
    min_len: u64,
    max_len: u64,
    total_len: u128,
    frame_count: u64,
    start: Instant,
}

impl FrameBuffer {
    fn new(term_sz: TermSize, fg: &[String], bg: &[String]) -> AppResult<Self> {
        let term_sz = term_sz.validate()?;
        let px_sz = PX.len() + bg[LAST_COLOR].len() + fg[LAST_COLOR].len();
        let screen_sz = px_sz * term_sz.width * (term_sz.height - 1);
        // Cursor positions for every row, initial colors, and the status line.
        let capacity = screen_sz + term_sz.height * 24 + term_sz.width + 128;
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
        })
    }

    fn draw_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    fn draw_fire(&mut self, fire: &Fire, fg: &[String], bg: &[String]) {
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

    fn draw_status(&mut self) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let fps = if elapsed > 0.0 {
            self.frame_count as f64 / elapsed
        } else {
            0.0
        };
        let status = format!(
            "mem: {} min / {} avg / {} max [ {:.2} fps ]",
            format_binary_bytes(self.min_len as f64),
            format_binary_bytes(self.average_len()),
            format_binary_bytes(self.max_len as f64),
            fps
        );
        self.draw_str(&format!("{CSI}{};1H{COLOR_DEF}", self.term_sz.height));
        // The status is ASCII. Leave the bottom-right cell unused to avoid wrapping.
        self.draw_str(&status[..status.len().min(self.term_sz.width - 1)]);
        self.draw_str(LINE_CLEAR_TO_EOL);
    }

    fn paint(&mut self, app: &mut App) -> AppResult<()> {
        self.record_frame(self.bytes.len() as u64);
        self.draw_status();
        app.emit_bytes(&self.bytes)?;
        app.stdout.flush()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

impl Rng {
    fn seeded() -> Self {
        Self {
            state: random_seed() | 1,
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_0_to_3(&mut self) -> usize {
        (self.next_u64() & 3) as usize
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("doom-fire-rs: {err}");
        process::exit(1);
    }
}

fn run() -> AppResult<()> {
    // Reject invalid streams before job control can suspend a background launch.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("stdin and stdout must be terminals".into());
    }
    INTERRUPTED.store(false, Ordering::Relaxed);
    platform::install_exit_handlers()?;

    #[cfg(unix)]
    platform::wait_for_foreground()?;
    if interrupted() {
        return Ok(());
    }

    let mut app = App::new()?;
    let result = app.run();
    let cleanup = app.complete();

    result?;
    cleanup?;
    Ok(())
}

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

// Consume exactly one complete line, without allocating an unbounded input buffer.
// In particular, keep a CRLF or an answer's remaining bytes out of the next prompt.
fn read_prompt(input: &mut impl BufRead) -> io::Result<bool> {
    let mut first = None;
    loop {
        let bytes = match input.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if bytes.is_empty() {
            return Ok(first.is_none() || first == Some(b'q'));
        }
        first = first.or_else(|| bytes.first().copied());
        if let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            input.consume(end + 1);
            return Ok(first == Some(b'q'));
        }
        let len = bytes.len();
        input.consume(len);
    }
}

fn init_colors(kind: &str) -> Vec<String> {
    (0..MAX_COLOR)
        .map(|idx| format!("{CSI}{kind}{idx}m"))
        .collect()
}

fn nl() -> &'static str {
    if cfg!(windows) { LINE_NEW } else { "\n" }
}

fn sep() -> &'static str {
    if cfg!(windows) { "|" } else { "\u{258f}" }
}

fn term_on() -> String {
    format!("{SCREEN_BUF_ON}{CURSOR_HIDE}{CURSOR_HOME}{COLOR_DEF}{SCREEN_CLEAR}")
}

fn term_off() -> String {
    format!("{COLOR_RESET}{CURSOR_SHOW}{SCREEN_BUF_OFF}")
}

fn format_binary_bytes(bytes: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes;
    let mut unit_idx = 0;

    while value >= 1024.0 && unit_idx + 1 < units.len() {
        value /= 1024.0;
        unit_idx += 1;
    }

    format!("{value:.2} {}", units[unit_idx])
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

#[cfg(unix)]
fn env_term_size() -> Option<TermSize> {
    let width = std::env::var("COLUMNS").ok()?.parse().ok()?;
    let height = std::env::var("LINES").ok()?.parse().ok()?;
    Some(TermSize { height, width })
}

#[cfg(unix)]
mod platform {
    use super::{INTERRUPTED, Ordering, TermSize, env_term_size};
    use std::{
        io,
        os::{
            fd::{AsRawFd, RawFd},
            raw::{c_int, c_ulong, c_ushort},
        },
    };

    pub struct Console;

    static SUSPEND_REQUESTED: super::AtomicBool = super::AtomicBool::new(false);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WinSize {
        ws_row: c_ushort,
        ws_col: c_ushort,
        ws_xpixel: c_ushort,
        ws_ypixel: c_ushort,
    }

    // These values are ABI-specific, including differences between Linux architectures.
    #[derive(Debug, PartialEq, Eq)]
    struct TerminalAbi {
        winsize: c_ulong,
        stop: c_int,
        suspend: c_int,
        output_on: c_int,
    }

    #[cfg(any(target_os = "linux", target_os = "android", test))]
    const fn linux_abi(arch: &str) -> TerminalAbi {
        let (winsize, stop, suspend) = match arch.as_bytes() {
            b"mips" | b"mips64" | b"mips32r6" | b"mips64r6" => (0x4008_7468, 23, 24),
            b"powerpc" | b"powerpc64" => (0x4008_7468, 19, 20),
            b"sparc" | b"sparc64" => (0x4008_7468, 17, 18),
            _ => (0x5413, 19, 20),
        };
        TerminalAbi {
            winsize,
            stop,
            suspend,
            output_on: 1,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    const ABI: TerminalAbi = linux_abi(std::env::consts::ARCH);

    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    const ABI: TerminalAbi = TerminalAbi {
        winsize: 0x4008_7468,
        stop: 17,
        suspend: 18,
        output_on: 2,
    };

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    compile_error!("terminal ABI bindings are required for this Unix target");

    unsafe extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
        fn signal(signal: c_int, handler: usize) -> usize;
        fn raise(signal: c_int) -> c_int;
        fn tcflow(fd: c_int, action: c_int) -> c_int;
        fn tcgetpgrp(fd: c_int) -> c_int;
        fn getpgrp() -> c_int;

        #[cfg_attr(
            any(target_os = "linux", target_os = "dragonfly"),
            link_name = "__errno_location"
        )]
        #[cfg_attr(
            any(target_os = "android", target_os = "openbsd", target_os = "netbsd"),
            link_name = "__errno"
        )]
        #[cfg_attr(
            any(target_vendor = "apple", target_os = "freebsd"),
            link_name = "__error"
        )]
        fn errno_location() -> *mut c_int;
    }

    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;
    const SIG_ERR: usize = usize::MAX;

    extern "C" fn handle_signal(sig: c_int) {
        if sig == ABI.suspend {
            SUSPEND_REQUESTED.store(true, Ordering::Relaxed);
        } else {
            INTERRUPTED.store(true, Ordering::Relaxed);
        }
        resume_output();
    }

    // Used both by signal handlers and normal cleanup (including q and EOF).
    pub fn resume_output() {
        // POSIX makes tcflow/tcgetpgrp/getpgrp async-signal-safe. Wake a write
        // blocked by Ctrl-S so the main thread can restore the screen. Preserve
        // errno and avoid changing another foreground job's output state.
        unsafe {
            let errno = errno_location();
            let saved = *errno;
            if can_access_terminal() {
                // Linux distinguishes a Ctrl-S pause from a TCOOFF pause:
                // TCOON alone only restarts the latter. Toggle both actions
                // to wake either kind of paused output before cleanup.
                tcflow(1, ABI.output_on - 1);
                tcflow(1, ABI.output_on);
            }
            *errno = saved;
        }
    }

    pub fn install_exit_handlers() -> io::Result<()> {
        SUSPEND_REQUESTED.store(false, Ordering::Relaxed);
        for sig in [SIGINT, SIGTERM, ABI.suspend] {
            if unsafe { signal(sig, handle_signal as *const () as usize) } == SIG_ERR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn take_suspend_request() -> bool {
        SUSPEND_REQUESTED.swap(false, Ordering::Relaxed)
    }

    fn can_access_terminal() -> bool {
        let foreground = unsafe { tcgetpgrp(1) };
        // A terminal descriptor can be usable without being our controlling
        // terminal (for example after setsid). There is no job-control group
        // to wait for in that case. Invalid descriptors fail in normal I/O.
        foreground < 0 || foreground == unsafe { getpgrp() }
    }

    pub fn wait_for_foreground() -> io::Result<()> {
        while !super::interrupted() && !can_access_terminal() {
            // Stop before writing anything, including the initial alternate
            // screen sequence. `bg` stops again; `fg` permits drawing.
            if unsafe { raise(ABI.stop) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn suspend_process() -> io::Result<()> {
        // The caller has flushed terminal restoration. SIGSTOP avoids races
        // from temporarily replacing the SIGTSTP handler with SIG_DFL.
        if unsafe { raise(ABI.stop) } != 0 {
            return Err(io::Error::last_os_error());
        }
        wait_for_foreground()
    }

    pub fn init_console() -> io::Result<Console> {
        Ok(Console)
    }

    pub fn term_size(_: &Console) -> io::Result<TermSize> {
        let stdout = io::stdout();
        if let Ok(Some(size)) = ioctl_term_size(stdout.as_raw_fd()) {
            return Ok(size);
        }

        Ok(env_term_size().unwrap_or_default())
    }

    fn ioctl_term_size(fd: RawFd) -> io::Result<Option<TermSize>> {
        let mut winsz = WinSize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let rv = unsafe { ioctl(fd, ABI.winsize, &mut winsz) };
        if rv < 0 {
            return Err(io::Error::last_os_error());
        }

        if winsz.ws_row == 0 || winsz.ws_col == 0 {
            return Ok(None);
        }

        Ok(Some(TermSize {
            height: winsz.ws_row as usize,
            width: winsz.ws_col as usize,
        }))
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn linux_terminal_abis_match_kernel_headers() {
            for arch in [
                "powerpc",
                "powerpc64",
                "mips",
                "mips64",
                "mips32r6",
                "mips64r6",
                "sparc",
                "sparc64",
            ] {
                assert_eq!(linux_abi(arch).winsize, 0x4008_7468);
            }
            assert_eq!(linux_abi("mips64").stop, 23);
            assert_eq!(linux_abi("mips64").suspend, 24);
            assert_eq!(linux_abi("sparc64").suspend, 18);
            for arch in ["x86", "x86_64", "aarch64", "arm", "riscv64", "loongarch64"] {
                assert_eq!(linux_abi(arch).winsize, 0x5413);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{INTERRUPTED, Ordering, TermSize};
    use std::{cmp, ffi::c_void, io, ptr};

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut c_void;
    type Short = i16;
    type Uint = u32;
    type Word = u16;

    pub struct Console {
        handle: Handle,
        original_mode: Dword,
        original_code_page: Uint,
    }

    impl Drop for Console {
        fn drop(&mut self) {
            unsafe {
                SetConsoleOutputCP(self.original_code_page);
                SetConsoleMode(self.handle, self.original_mode);
            }
        }
    }

    const STD_OUTPUT_HANDLE: Dword = -11_i32 as Dword;
    const ENABLE_PROCESSED_OUTPUT: Dword = 0x0001;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: Dword = 0x0004;
    const DISABLE_NEWLINE_AUTO_RETURN: Dword = 0x0008;
    const CP_UTF8: Uint = 65001;
    const CTRL_C_EVENT: Dword = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord {
        x: Short,
        y: Short,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SmallRect {
        left: Short,
        top: Short,
        right: Short,
        bottom: Short,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ConsoleScreenBufferInfo {
        dw_size: Coord,
        dw_cursor_position: Coord,
        w_attributes: Word,
        sr_window: SmallRect,
        dw_maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: Dword) -> Handle;
        fn GetConsoleMode(hConsoleHandle: Handle, lpMode: *mut Dword) -> Bool;
        fn SetConsoleMode(hConsoleHandle: Handle, dwMode: Dword) -> Bool;
        fn GetConsoleOutputCP() -> Uint;
        fn SetConsoleOutputCP(wCodePageID: Uint) -> Bool;
        fn GetConsoleScreenBufferInfo(
            hConsoleOutput: Handle,
            lpConsoleScreenBufferInfo: *mut ConsoleScreenBufferInfo,
        ) -> Bool;
        fn WriteConsoleA(
            hConsoleOutput: Handle,
            lpBuffer: *const c_void,
            nNumberOfCharsToWrite: Dword,
            lpNumberOfCharsWritten: *mut Dword,
            lpReserved: *mut c_void,
        ) -> Bool;
        fn SetConsoleCtrlHandler(
            HandlerRoutine: Option<unsafe extern "system" fn(Dword) -> Bool>,
            Add: Bool,
        ) -> Bool;
    }

    unsafe extern "system" fn handle_ctrl_c(ctrl_type: Dword) -> Bool {
        if ctrl_type != CTRL_C_EVENT {
            return 0;
        }

        INTERRUPTED.store(true, Ordering::Relaxed);
        1
    }

    pub fn install_exit_handlers() -> io::Result<()> {
        if unsafe { SetConsoleCtrlHandler(Some(handle_ctrl_c), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    pub fn init_console() -> io::Result<Console> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle.is_null() || handle == (-1_isize as Handle) {
            return Err(io::Error::last_os_error());
        }

        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let original_code_page = unsafe { GetConsoleOutputCP() };
        if original_code_page == 0 {
            return Err(io::Error::last_os_error());
        }
        // Create the guard before the first mutation so setup failures also restore state.
        let console = Console {
            handle,
            original_mode: mode,
            original_code_page,
        };
        mode |= ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            | DISABLE_NEWLINE_AUTO_RETURN;
        if unsafe { SetConsoleMode(handle, mode) } == 0 {
            return Err(io::Error::last_os_error());
        }

        if unsafe { SetConsoleOutputCP(CP_UTF8) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(console)
    }

    pub fn term_size(console: &Console) -> io::Result<TermSize> {
        let mut info = ConsoleScreenBufferInfo {
            dw_size: Coord { x: 0, y: 0 },
            dw_cursor_position: Coord { x: 0, y: 0 },
            w_attributes: 0,
            sr_window: SmallRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dw_maximum_window_size: Coord { x: 0, y: 0 },
        };

        if unsafe { GetConsoleScreenBufferInfo(console.handle, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(TermSize {
            height: (i32::from(info.sr_window.bottom) - i32::from(info.sr_window.top) + 1) as usize,
            width: (i32::from(info.sr_window.right) - i32::from(info.sr_window.left) + 1) as usize,
        })
    }

    pub fn write_console(console: &Console, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let chunk_len = cmp::min(bytes.len(), Dword::MAX as usize);
            let mut written = 0;
            let ok = unsafe {
                WriteConsoleA(
                    console.handle,
                    bytes.as_ptr().cast(),
                    chunk_len as Dword,
                    &mut written,
                    ptr::null_mut(),
                )
            };

            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "WriteConsoleA wrote zero bytes",
                ));
            }

            bytes = &bytes[written as usize..];
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod allocations {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

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
            COUNT.set(Some(0));
            f();
            COUNT.take().unwrap()
        }
    }

    #[test]
    fn prompts_consume_complete_lines_including_crlf() {
        for input in [b"x\nq\n".as_slice(), b"\r\nq\r\n", b"continue\nq\n"] {
            // Tiny buffers also exercise lines and CRLF split across reads.
            let mut reader = io::BufReader::with_capacity(1, input);
            assert!(!read_prompt(&mut reader).unwrap());
            assert!(read_prompt(&mut reader).unwrap());
            assert!(read_prompt(&mut reader).unwrap()); // EOF
        }
    }

    #[test]
    fn drawing_fire_does_not_allocate_after_initialization() {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        for (width, height) in [(120, 24), (200, 100)] {
            let size = TermSize { width, height };
            let fire = Fire::new(size).unwrap();
            let mut frame = FrameBuffer::new(size, &fg, &bg).unwrap();
            assert_eq!(allocations::count(|| frame.draw_fire(&fire, &fg, &bg)), 0);
        }
    }

    #[test]
    fn rng_range_is_bounded() {
        let mut rng = Rng { state: 1 };
        for _ in 0..1_000 {
            assert!(rng.next_0_to_3() <= 3);
        }
    }

    #[test]
    fn fire_palette_indexes_fit_color_table() {
        assert!(FIRE_PALETTE.iter().all(|idx| *idx < MAX_COLOR));
    }

    #[test]
    fn formats_binary_bytes() {
        assert_eq!(format_binary_bytes(0.0), "0.00 B");
        assert_eq!(format_binary_bytes(1024.0), "1.00 KiB");
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
            let mut rng = Rng { state: 1 };
            for _ in 0..100 {
                fire.advance(&mut rng);
            }
            assert!(
                fire.pixels
                    .iter()
                    .all(|&px| usize::from(px) < FIRE_PALETTE.len())
            );
            let mut frame = FrameBuffer::new(size, &fg, &bg).unwrap();
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
        let mut frame = FrameBuffer::new(size, &fg, &bg).unwrap();
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
        )
        .unwrap();
        assert_eq!(frame.average_len(), 0.0);
        frame.record_frame(100);
        for _ in 1..1000 {
            frame.record_frame(200);
        }
        assert_eq!(frame.min_len, 100);
        assert_eq!(frame.max_len, 200);
        assert_eq!(format_binary_bytes(frame.average_len()), "199.90 B");
    }
}
