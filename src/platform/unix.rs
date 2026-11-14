use super::INTERRUPTED;
use crate::terminal::TermSize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    io,
    os::{
        fd::{AsRawFd, RawFd},
        raw::{c_int, c_ulong, c_ushort},
    },
};

pub(crate) struct Console;

pub(crate) struct Keyboard {
    original: libc::termios,
    worker: Option<super::InputWorker>,
}

impl Keyboard {
    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.worker
            .take()
            .map_or(Ok(()), |mut worker| worker.finish())
    }

    pub(crate) fn start() -> io::Result<Self> {
        let mut original = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(0, original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { original.assume_init() };
        let mut mode = original;
        // Keep signals and flow control, but deliver q without Return or echo.
        mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ECHONL);
        mode.c_cc[libc::VMIN] = 0;
        mode.c_cc[libc::VTIME] = 1;
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut keyboard = Self {
            original,
            worker: None,
        };
        keyboard.worker = Some(super::InputWorker::start(|| {
            use std::io::Read;
            let mut byte = [0];
            match io::stdin().read(&mut byte) {
                Ok(1) => Ok(byte[0] == b'q'),
                Ok(_) => Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
                Err(error) => Err(error),
            }
        })?);
        Ok(keyboard)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // Stop reading before returning input ownership to the shell.
        drop(self.worker.take());
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.original);
        }
    }
}

static SUSPEND_REQUESTED: AtomicBool = AtomicBool::new(false);

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
pub(crate) fn resume_output() {
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

pub(crate) fn install_exit_handlers() -> io::Result<()> {
    SUSPEND_REQUESTED.store(false, Ordering::Relaxed);
    for sig in [SIGINT, SIGTERM, ABI.suspend] {
        if unsafe { signal(sig, handle_signal as *const () as usize) } == SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub(crate) fn take_suspend_request() -> bool {
    SUSPEND_REQUESTED.swap(false, Ordering::Relaxed)
}

fn can_access_terminal() -> bool {
    let foreground = unsafe { tcgetpgrp(1) };
    // A terminal descriptor can be usable without being our controlling
    // terminal (for example after setsid). There is no job-control group
    // to wait for in that case. Invalid descriptors fail in normal I/O.
    foreground < 0 || foreground == unsafe { getpgrp() }
}

pub(crate) fn wait_for_foreground() -> io::Result<()> {
    while !super::interrupted() && !can_access_terminal() {
        // Stop before writing anything, including the initial alternate
        // screen sequence. `bg` stops again; `fg` permits drawing.
        if unsafe { raise(ABI.stop) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub(crate) fn suspend_process() -> io::Result<()> {
    // The caller has flushed terminal restoration. SIGSTOP avoids races
    // from temporarily replacing the SIGTSTP handler with SIG_DFL.
    if unsafe { raise(ABI.stop) } != 0 {
        return Err(io::Error::last_os_error());
    }
    wait_for_foreground()
}

pub(crate) fn init_console() -> io::Result<Console> {
    Ok(Console)
}

pub(crate) fn term_size(_: &Console) -> io::Result<TermSize> {
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
fn env_term_size() -> Option<TermSize> {
    let width = std::env::var("COLUMNS").ok()?.parse().ok()?;
    let height = std::env::var("LINES").ok()?.parse().ok()?;
    Some(TermSize { height, width })
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
