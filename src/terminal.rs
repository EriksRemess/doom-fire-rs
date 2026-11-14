use std::io;

pub(crate) const MAX_COLOR: usize = 256;
pub(crate) const LAST_COLOR: usize = MAX_COLOR - 1;
// Bound both the simulation and the worst-case encoded frame to a few dozen MiB.
const MAX_TERM_CELLS: usize = 1_000_000;

pub(crate) const CSI: &str = "\x1B[";
pub(crate) const LINE_CLEAR_TO_EOL: &str = "\x1B[0K";
pub(crate) const LINE_NEW: &str = "\x1B[0K\x1B[1E\x1B[1G";
pub(crate) const CURSOR_SAVE: &str = "\x1B7";
pub(crate) const CURSOR_LOAD: &str = "\x1B8";
pub(crate) const CURSOR_SHOW: &str = "\x1B[?25h";
pub(crate) const CURSOR_HIDE: &str = "\x1B[?25l";
pub(crate) const CURSOR_HOME: &str = "\x1B[1;1H";
pub(crate) const SCREEN_CLEAR: &str = "\x1B[2J";
pub(crate) const SCREEN_BUF_ON: &str = "\x1B[?1049h";
pub(crate) const SCREEN_BUF_OFF: &str = "\x1B[?1049l";
pub(crate) const CHAR_SET_ASCII: &str = "\x1B(B";
pub(crate) const COLOR_RESET: &str = "\x1B[0m";
pub(crate) const COLOR_DEF: &str = "\x1B[39;49m";
pub(crate) const COLOR_ITALIC: &str = "\x1B[3m";
pub(crate) const COLOR_NOT_ITALIC: &str = "\x1B[23m";
pub(crate) const PX: &str = "\u{2580}";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TermSize {
    pub(crate) height: usize,
    pub(crate) width: usize,
}

impl TermSize {
    pub(crate) fn validate(self) -> io::Result<Self> {
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

pub(crate) fn init_colors(kind: &str) -> Vec<String> {
    Vec::from_fn(MAX_COLOR, |idx| format!("{CSI}{kind}{idx}m"))
}

pub(crate) fn nl() -> &'static str {
    if cfg!(windows) { LINE_NEW } else { "\n" }
}

pub(crate) fn sep() -> &'static str {
    if cfg!(windows) { "|" } else { "\u{258f}" }
}

pub(crate) fn term_on() -> String {
    format!("{SCREEN_BUF_ON}{CURSOR_HIDE}{CURSOR_HOME}{COLOR_DEF}{SCREEN_CLEAR}")
}

pub(crate) fn term_off() -> String {
    format!("{COLOR_RESET}{CURSOR_SHOW}{SCREEN_BUF_OFF}")
}
