use std::{
    env, fmt,
    fs::File,
    io::{self, Read, Write},
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
const COLOR_FG_DEF: &str = "\x1B[38;5;15m";
const COLOR_BG_DEF: &str = "\x1B[48;5;0m";
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

#[derive(Clone, Copy, Debug, Default)]
struct TermSize {
    height: usize,
    width: usize,
}

struct App {
    stdout: io::Stdout,
    #[cfg_attr(unix, allow(dead_code))]
    console: platform::Console,
    term_sz: TermSize,
    fg: Vec<String>,
    bg: Vec<String>,
    rng: Rng,
}

impl App {
    fn new() -> AppResult<Self> {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        let console = platform::init_console()?;
        let term_sz = platform::term_size(&console)?;
        let rng = Rng::seeded();

        let mut app = Self {
            stdout: io::stdout(),
            console,
            term_sz,
            fg,
            bg,
            rng,
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

        self.show_term_capabilities()?;
        if interrupted() {
            return Ok(());
        }

        self.show_doom_fire()
    }

    fn complete(&mut self) -> io::Result<()> {
        self.emit(&term_off())?;

        if interrupted() {
            return self.stdout.flush();
        }

        self.emit("Complete!")?;
        self.emit(nl())?;
        self.stdout.flush()
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
            if platform::write_console(&self.console, bytes).is_ok() {
                return Ok(());
            }
        }

        self.stdout.write_all(bytes)
    }

    fn pause(&mut self) -> AppResult<()> {
        self.emit(COLOR_RESET)?;
        self.emit("Press return to continue...")?;
        self.stdout.flush()?;

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            let result =
                io::stdin().read(&mut byte).map(
                    |bytes_read| {
                        if bytes_read == 1 { Some(byte[0]) } else { None }
                    },
                );
            let _ = sender.send(result);
        });

        while !interrupted() {
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(Some(b'q'))) => {
                    self.complete()?;
                    process::exit(0);
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
        } else if width == 0 {
            self.emit_bg(1)?;
            self.emit_fg(15)?;
            self.emit_fmt(format_args!(
                "Call to retreive terminal dimensions may have failed{}Width is {} (ZERO!) and we need {}.{}We will allocate 0 bytes of screen buffer, resulting in immediate failure.",
                nl(),
                width,
                min_w,
                nl()
            ))?;
            self.emit(COLOR_RESET)?;
        } else if height == 0 {
            self.emit_bg(1)?;
            self.emit_fg(15)?;
            self.emit_fmt(format_args!(
                "Call to retreive terminal dimensions may have failed{}Height is {} (ZERO!) and we need {}.{}We will allocate 0 bytes of screen buffer, resulting in immediate failure.",
                nl(),
                height,
                min_h,
                nl()
            ))?;
            self.emit(COLOR_RESET)?;
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

        self.pause()?;

        self.emit(COLOR_RESET)?;
        self.emit(CURSOR_HOME)?;
        self.emit(SCREEN_CLEAR)?;

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

    fn scroll_marquee(&mut self) -> AppResult<()> {
        let bg_idx = 222;
        let marquee_row = if cfg!(windows) {
            nl().to_string()
        } else {
            format!("{LINE_CLEAR_TO_EOL}{}", nl())
        };
        let marquee_bg = marquee_row.repeat(4);

        self.emit(CURSOR_SAVE)?;
        self.emit_bg(bg_idx)?;
        self.emit(&marquee_bg)?;

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

                interruptible_sleep(Duration::from_millis(10));
            }

            interruptible_sleep(Duration::from_millis(1_000));

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

                interruptible_sleep(Duration::from_millis(10));
            }

            self.emit(nl())?;
        }

        Ok(())
    }

    fn show_term_capabilities(&mut self) -> AppResult<()> {
        self.show_term_size()?;
        self.show_standard_colors()?;
        self.show_216_colors()?;
        self.show_grayscale()?;
        self.scroll_marquee()?;
        if interrupted() { Ok(()) } else { self.pause() }
    }

    fn show_doom_fire(&mut self) -> AppResult<()> {
        let fire_h = self.term_sz.height * 2;
        let fire_w = self.term_sz.width;
        if fire_h == 0 || fire_w == 0 {
            return Err("terminal size is zero; cannot render fire".into());
        }

        let fire_sz = fire_h * fire_w;
        let fire_last_row = (fire_h - 1) * fire_w;
        let fire_black = 0_u8;
        let fire_white = (FIRE_PALETTE.len() - 1) as u8;

        let mut screen_buf = vec![fire_black; fire_sz];
        for x in 0..fire_w {
            screen_buf[fire_last_row + x] = fire_white;
        }

        self.emit(CURSOR_HOME)?;
        self.emit(COLOR_RESET)?;
        self.emit(COLOR_BG_DEF)?;
        self.emit(COLOR_FG_DEF)?;
        self.emit(SCREEN_CLEAR)?;

        let init_frame = format!("{CURSOR_HOME}{}{}", self.bg[0], self.fg[0]);
        let mut frame = FrameBuffer::new(self.term_sz, &self.fg, &self.bg);

        while !interrupted() {
            for x in 0..fire_w {
                for y in 0..fire_h {
                    let fire_idx = y * fire_w + x;
                    let spread_px = screen_buf[fire_idx];

                    if spread_px == 0 && fire_idx >= fire_w {
                        screen_buf[fire_idx - fire_w] = 0;
                    } else {
                        let spread_rnd_idx = self.rng.next_0_to_3();
                        let spread_dst = if fire_idx >= spread_rnd_idx + 1 {
                            fire_idx - spread_rnd_idx + 1
                        } else {
                            fire_idx
                        };

                        if spread_dst >= fire_w {
                            let decay = (spread_rnd_idx & 1) as u8;
                            screen_buf[spread_dst - fire_w] = if spread_px > decay {
                                spread_px - decay
                            } else {
                                0
                            };
                        }
                    }
                }
            }

            frame.reset();
            frame.draw_str(&init_frame);

            {
                let fg = &self.fg;
                let bg = &self.bg;
                let mut px_prev_hi = fire_black;
                let mut px_prev_lo = fire_black;

                for y in (0..fire_h).step_by(2) {
                    for x in 0..fire_w {
                        let px_hi = screen_buf[y * fire_w + x];
                        let px_lo = screen_buf[(y + 1) * fire_w + x];

                        if px_lo != px_prev_lo {
                            frame.draw_str(&bg[FIRE_PALETTE[px_lo as usize]]);
                        }
                        if px_hi != px_prev_hi {
                            frame.draw_str(&fg[FIRE_PALETTE[px_hi as usize]]);
                        }
                        frame.draw_str(PX);

                        px_prev_hi = px_hi;
                        px_prev_lo = px_lo;
                    }
                    frame.draw_str(nl());
                }
            }

            frame.paint(self)?;
            frame.reset();
        }

        Ok(())
    }
}

struct FrameBuffer {
    bytes: Vec<u8>,
    min_len: u64,
    max_len: u64,
    avg_len: u64,
    frame_count: u64,
    start: Instant,
}

impl FrameBuffer {
    fn new(term_sz: TermSize, fg: &[String], bg: &[String]) -> Self {
        let px_char_sz = PX.len();
        let px_color_sz = bg[LAST_COLOR].len() + fg[LAST_COLOR].len();
        let px_sz = px_color_sz + px_char_sz;
        let screen_sz = px_sz * term_sz.width * term_sz.width;
        let overflow_sz = px_char_sz * 100;

        Self {
            bytes: Vec::with_capacity((screen_sz + overflow_sz) * 2),
            min_len: 0,
            max_len: 0,
            avg_len: 0,
            frame_count: 0,
            start: Instant::now(),
        }
    }

    fn reset(&mut self) {
        self.bytes.clear();
    }

    fn draw_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    fn paint(&mut self, app: &mut App) -> AppResult<()> {
        let emit_len = self
            .bytes
            .strip_suffix(nl().as_bytes())
            .map_or(self.bytes.len(), <[u8]>::len);
        app.emit_bytes(&self.bytes[..emit_len])?;

        let frame_len = self.bytes.len() as u64;
        self.frame_count += 1;

        if self.min_len == 0 {
            self.min_len = frame_len;
            self.max_len = frame_len;
            self.avg_len = frame_len;
        } else {
            self.min_len = self.min_len.min(frame_len);
            self.max_len = self.max_len.max(frame_len);
            self.avg_len = ((self.avg_len as u128 * (self.frame_count - 1) as u128
                + frame_len as u128)
                / self.frame_count as u128) as u64;
        }

        let elapsed = self.start.elapsed().as_secs_f64();
        let fps = if elapsed > 0.0 {
            self.frame_count as f64 / elapsed
        } else {
            0.0
        };

        app.emit_fg(0)?;
        app.emit_fmt(format_args!(
            "mem: {} min / {} avg / {} max [ {:.2} fps ]",
            format_binary_bytes(self.min_len),
            format_binary_bytes(self.avg_len),
            format_binary_bytes(self.max_len),
            fps
        ))?;
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
    INTERRUPTED.store(false, Ordering::Relaxed);
    platform::install_ctrl_c_handler()?;

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

fn interruptible_sleep(duration: Duration) {
    let deadline = Instant::now() + duration;

    while !interrupted() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        thread::sleep(remaining.min(Duration::from_millis(25)));
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

fn format_binary_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
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
        let mut bytes = [0_u8; 8];
        if File::open("/dev/urandom")
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

fn env_term_size() -> Option<TermSize> {
    let width = env::var("COLUMNS").ok()?.parse().ok()?;
    let height = env::var("LINES").ok()?.parse().ok()?;
    Some(TermSize { height, width })
}

#[cfg(unix)]
mod platform {
    use super::{INTERRUPTED, Ordering, TermSize, env_term_size};
    use std::{
        fs::File,
        io,
        os::{
            fd::{AsRawFd, RawFd},
            raw::{c_int, c_ulong, c_ushort},
        },
    };

    pub struct Console;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WinSize {
        ws_row: c_ushort,
        ws_col: c_ushort,
        ws_xpixel: c_ushort,
        ws_ypixel: c_ushort,
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    const TIOCGWINSZ: c_ulong = 0x5413;

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    const TIOCGWINSZ: c_ulong = 0x4008_7468;

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    const TIOCGWINSZ: c_ulong = 0x5413;

    unsafe extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
        fn signal(signal: c_int, handler: usize) -> usize;
    }

    const SIGINT: c_int = 2;
    const SIG_ERR: usize = usize::MAX;

    extern "C" fn handle_sigint(_: c_int) {
        INTERRUPTED.store(true, Ordering::Relaxed);
    }

    pub fn install_ctrl_c_handler() -> io::Result<()> {
        if unsafe { signal(SIGINT, handle_sigint as *const () as usize) } == SIG_ERR {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    pub fn init_console() -> io::Result<Console> {
        Ok(Console)
    }

    pub fn term_size(_: &Console) -> io::Result<TermSize> {
        let stdout = io::stdout();
        if let Ok(Some(size)) = ioctl_term_size(stdout.as_raw_fd()) {
            return Ok(size);
        }

        if let Ok(tty) = File::open("/dev/tty") {
            if let Ok(Some(size)) = ioctl_term_size(tty.as_raw_fd()) {
                return Ok(size);
            }
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

        let rv = unsafe { ioctl(fd, TIOCGWINSZ, &mut winsz) };
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
    }

    const STD_OUTPUT_HANDLE: Dword = -11_i32 as Dword;
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

    pub fn install_ctrl_c_handler() -> io::Result<()> {
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

        mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
        if unsafe { SetConsoleMode(handle, mode) } == 0 {
            return Err(io::Error::last_os_error());
        }

        if unsafe { SetConsoleOutputCP(CP_UTF8) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Console { handle })
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
        assert_eq!(format_binary_bytes(0), "0.00 B");
        assert_eq!(format_binary_bytes(1024), "1.00 KiB");
    }
}
