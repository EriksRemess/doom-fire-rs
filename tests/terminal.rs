//! Unix pseudo-terminal regression tests, run automatically by `cargo test`.
#![cfg(unix)]

use std::{
    fs::File,
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, ExitStatus, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_doom-fire-rs");
const PROMPT: &[u8] = b"Press return to continue...";
const HOME: &[u8] = b"\x1b[1;1H";
const OFF: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?1049l";
const ON: &[u8] = b"\x1b[?1049h";
const PX: &[u8] = "▀".as_bytes();
const FRAME_END: &[u8] = b" fps ]\x1b[0K";

// openpty has no portable atomic CLOEXEC option. Exclude every child spawn
// until both new descriptors have been marked close-on-exec.
static PTY_SPAWN_LOCK: Mutex<()> = Mutex::new(());

fn spawn_command(command: &mut Command) -> io::Result<Child> {
    let _guard = PTY_SPAWN_LOCK.lock().unwrap();
    command.spawn()
}

fn bash_available() -> bool {
    match spawn_command(Command::new("bash").arg("--version").stdout(Stdio::null())) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipping job-control coverage: Bash is unavailable");
            false
        }
        result => {
            assert!(result.unwrap().wait().unwrap().success());
            true
        }
    }
}

fn position(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
    bytes
        .windows(pattern.len())
        .position(|window| window == pattern)
}

fn contains(bytes: &[u8], pattern: &[u8]) -> bool {
    position(bytes, pattern).is_some()
}

fn count(bytes: &[u8], pattern: &[u8]) -> usize {
    bytes
        .windows(pattern.len())
        .filter(|window| *window == pattern)
        .count()
}

fn last_frame(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .windows(HOME.len())
        .rposition(|window| window == HOME)
        .expect("frame must position the cursor at home");
    &bytes[start + HOME.len()..]
}

fn checked(result: libc::c_int) -> io::Result<()> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pty(rows: u16, columns: u16) -> (File, File) {
    let _guard = PTY_SPAWN_LOCK.lock().unwrap();
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // openpty initializes both descriptors on success. File takes sole ownership.
    unsafe {
        checked(libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        ))
        .unwrap();
        let master = File::from_raw_fd(master);
        let slave = File::from_raw_fd(slave);
        // Tests run concurrently: never leak another test's PTY through exec.
        checked(libc::fcntl(
            master.as_raw_fd(),
            libc::F_SETFD,
            libc::FD_CLOEXEC,
        ))
        .unwrap();
        checked(libc::fcntl(
            slave.as_raw_fd(),
            libc::F_SETFD,
            libc::FD_CLOEXEC,
        ))
        .unwrap();
        (master, slave)
    }
}

struct Terminal {
    master: File,
    process: Child,
    buffer: Vec<u8>,
}

impl Terminal {
    fn new(rows: u16, columns: u16) -> Self {
        Self::spawn(rows, columns, Command::new(BINARY), None)
    }

    fn spawn(
        rows: u16,
        columns: u16,
        command: Command,
        memory_limit: Option<libc::rlim_t>,
    ) -> Self {
        Self::spawn_with_session(rows, columns, command, memory_limit, true)
    }

    fn spawn_with_session(
        rows: u16,
        columns: u16,
        mut command: Command,
        memory_limit: Option<libc::rlim_t>,
        controlling_terminal: bool,
    ) -> Self {
        let (master, slave) = pty(rows, columns);
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        // Only async-signal-safe libc calls run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                checked(libc::setsid())?;
                if controlling_terminal {
                    checked(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
                }
                let core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                checked(libc::setrlimit(libc::RLIMIT_CORE, &core))?;
                if let Some(limit) = memory_limit {
                    let memory = libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    };
                    checked(libc::setrlimit(libc::RLIMIT_AS, &memory))?;
                }
                Ok(())
            });
        }
        let process = spawn_command(&mut command).unwrap();
        Self {
            master,
            process,
            buffer: Vec::new(),
        }
    }

    fn resize(&self, rows: u16, columns: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // ioctl reads a winsize from this valid pointer.
        unsafe {
            checked(libc::ioctl(
                self.master.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &size,
            ))
            .unwrap();
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    fn signal(&self, signal: libc::c_int) {
        // Child remains owned and unreaped, so its PID cannot be reused.
        unsafe {
            checked(libc::kill(self.process.id() as _, signal)).unwrap();
        }
    }

    fn read_chunk(&mut self, deadline: Instant) -> bool {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for terminal output: {:?}",
                String::from_utf8_lossy(&self.buffer[self.buffer.len().saturating_sub(500)..])
            );
            let mut fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // poll borrows one initialized descriptor for the duration of the call.
            let ready = unsafe {
                libc::poll(
                    &mut fd,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as _,
                )
            };
            if ready == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                panic!("poll: {error}");
            }
            if ready == 0 {
                continue;
            }
            let mut chunk = [0; 65536];
            match self.master.read(&mut chunk) {
                Ok(0) => return false,
                Ok(length) => {
                    self.buffer.extend_from_slice(&chunk[..length]);
                    return true;
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return false,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }

    fn expect(&mut self, pattern: &[u8]) -> Vec<u8> {
        self.expect_until(pattern, Instant::now() + Duration::from_secs(8))
    }

    fn expect_until(&mut self, pattern: &[u8], deadline: Instant) -> Vec<u8> {
        loop {
            if let Some(start) = position(&self.buffer, pattern) {
                return self.buffer.drain(..start + pattern.len()).collect();
            }
            assert!(
                self.read_chunk(deadline),
                "unexpected EOF waiting for {:?}: {:?}",
                String::from_utf8_lossy(pattern),
                String::from_utf8_lossy(&self.buffer)
            );
        }
    }

    fn finish(&mut self) -> (ExitStatus, Vec<u8>) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.read_chunk(deadline) {}
        let status = loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not exit after closing terminal"
            );
            thread::sleep(Duration::from_millis(10));
        };
        (status, std::mem::take(&mut self.buffer))
    }

    fn assert_restored(&mut self, code: i32) -> Vec<u8> {
        let (status, output) = self.finish();
        assert_eq!(
            status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(count(&output, OFF), 1);
        output
    }

    fn modes(&self) -> libc::termios {
        let mut modes = MaybeUninit::uninit();
        // tcgetattr initializes termios on success.
        unsafe {
            checked(libc::tcgetattr(self.master.as_raw_fd(), modes.as_mut_ptr())).unwrap();
            modes.assume_init()
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // A failed job-control test may leave an app in the shell's foreground group.
        // Kill that group before the shell, then reap the directly owned child.
        unsafe {
            let foreground = libc::tcgetpgrp(self.master.as_raw_fd());
            if foreground > 0 && foreground != libc::getpgrp() {
                libc::kill(-foreground, libc::SIGKILL);
            }
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn signals_and_quit_at_prompt_restore_screen() {
    for action in [b"\x03".as_slice(), b"", b"q\n", b"\x04"] {
        let mut terminal = Terminal::new(24, 80);
        terminal.expect(PROMPT);
        if action.is_empty() {
            terminal.signal(libc::SIGTERM);
        } else {
            terminal.send(action);
        }
        terminal.assert_restored(0);
    }
}

#[test]
fn one_line_does_not_answer_two_prompts() {
    let mut terminal = Terminal::new(24, 80);
    terminal.expect(PROMPT);
    terminal.send(b"x\n");
    terminal.expect(PROMPT);
    terminal.send(b"q\n");
    assert!(!contains(&terminal.assert_restored(0), PX));
}

#[test]
fn sigterm_resumes_flow_control_for_cleanup() {
    for fire in [false, true] {
        let mut terminal = Terminal::new(24, if fire { 120 } else { 80 });
        let original = terminal.modes();
        assert_ne!(original.c_iflag & libc::IXON, 0);
        terminal.expect(PROMPT);
        if fire {
            terminal.send(b"\n");
            terminal.expect(FRAME_END);
        }
        terminal.send(b"\x13");
        thread::sleep(Duration::from_millis(100));
        terminal.signal(libc::SIGTERM);
        terminal.assert_restored(0);
        let restored = terminal.modes();
        assert_eq!(restored.c_iflag, original.c_iflag);
        assert_eq!(restored.c_oflag, original.c_oflag);
        assert_eq!(restored.c_cflag, original.c_cflag);
        assert_eq!(restored.c_lflag, original.c_lflag);
        assert_eq!(restored.c_cc, original.c_cc);
        // Compare speeds through the portable accessors, avoiding struct padding.
        unsafe {
            assert_eq!(libc::cfgetispeed(&restored), libc::cfgetispeed(&original));
            assert_eq!(libc::cfgetospeed(&restored), libc::cfgetospeed(&original));
        }
    }
}

#[test]
fn suspend_and_fg_restore_and_redraw_each_screen() {
    if !bash_available() {
        return;
    }
    let shell_prompt = b"JOBTEST> ";
    for stage in ["warning", "marquee", "preview", "fire"] {
        let mut shell = Command::new("bash");
        shell
            .args(["--noprofile", "--norc", "-i"])
            .env("PS1", "JOBTEST> ")
            .env("HISTFILE", "/dev/null")
            .env("LC_ALL", "C");
        let mut terminal =
            Terminal::spawn(24, if stage == "warning" { 80 } else { 120 }, shell, None);
        terminal.expect(shell_prompt);
        terminal.send(format!("'{}'\n", BINARY.replace('\'', "'\\''")).as_bytes());
        terminal.expect(if stage == "marquee" {
            b"Things move along"
        } else {
            PROMPT
        });
        if stage == "fire" {
            terminal.send(b"\n");
            terminal.expect(FRAME_END);
            terminal.send(b"\x13");
        }
        terminal.send(b"\x1a");
        let stopped = terminal.expect(shell_prompt);
        assert!(position(&stopped, OFF).unwrap() < position(&stopped, b"Stopped").unwrap());
        if stage == "warning" || stage == "fire" {
            terminal.resize(30, 160);
        }
        if stage == "fire" {
            terminal.send(b"bg\n");
            assert!(!contains(&terminal.expect(shell_prompt), ON));
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                // A job notification can redraw PS1 before this command runs.
                // Use a marker absent from the echoed command to delimit its output.
                terminal.send(b"jobs -s; printf '\\112OBSTATUS\\n'\n");
                let background = terminal.expect_until(b"JOBSTATUS\r\n", deadline);
                assert!(!contains(&background, ON));
                terminal.expect_until(shell_prompt, deadline);
                if contains(&background, b"Stopped") {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        terminal.send(b"fg\n");
        terminal.expect(ON);
        let resumed = terminal.expect(if stage == "fire" { FRAME_END } else { PROMPT });
        if stage == "fire" {
            assert_eq!(count(last_frame(&resumed), PX), 160 * 29);
        } else {
            assert!(contains(&resumed, b"Screen"));
            if stage == "warning" {
                assert!(contains(&resumed, b"Screen size: 160w x 30h"));
                assert!(!contains(&resumed, b"too small"));
            }
        }
        terminal.send(b"\x03");
        assert!(contains(&terminal.expect(shell_prompt), OFF));
        terminal.send(b"exit\n");
        terminal.finish();
    }
}

#[test]
fn frames_resize_and_sigterm_restores_screen() {
    let mut terminal = Terminal::new(24, 120);
    terminal.expect(PROMPT);
    terminal.send(b"\n");
    let output = terminal.expect(FRAME_END);
    let frame = last_frame(&output);
    assert_eq!(count(frame, PX), 120 * 23);
    assert!(contains(frame, b"\x1b[24;1H"));
    assert!(!frame.contains(&b'\n'));
    terminal.resize(30, 160);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = terminal.expect_until(FRAME_END, deadline);
        let frame = last_frame(&output);
        if contains(frame, b"\x1b[30;1H") {
            assert_eq!(count(frame, PX), 160 * 29);
            assert!(!frame.contains(&b'\n'));
            break;
        }
    }
    terminal.signal(libc::SIGTERM);
    terminal.assert_restored(0);
}

#[test]
fn invalid_resize_restores_screen_before_error() {
    let mut terminal = Terminal::new(24, 120);
    terminal.expect(PROMPT);
    terminal.send(b"\n");
    terminal.expect(FRAME_END);
    terminal.resize(1, 120);
    let output = terminal.assert_restored(1);
    assert!(contains(&output, b"at least 1 column by 2 rows"));
    assert!(position(&output, OFF).unwrap() < position(&output, b"doom-fire-rs:").unwrap());
}

#[test]
fn wide_terminal_runs_under_memory_limit() {
    let mut terminal = Terminal::spawn(22, 2000, Command::new(BINARY), Some(128 * 1024 * 1024));
    terminal.expect(PROMPT);
    terminal.send(b"\n");
    assert_eq!(
        count(last_frame(&terminal.expect(FRAME_END)), PX),
        2000 * 21
    );
    terminal.signal(libc::SIGINT);
    terminal.assert_restored(0);
}

#[test]
fn bad_dimensions_fail_before_entering_alternate_screen() {
    for (columns, lines) in [
        ("18446744073709551615", "22"),
        ("120", "0"),
        ("1001", "1000"),
    ] {
        let mut command = Command::new(BINARY);
        command.env("COLUMNS", columns).env("LINES", lines);
        let mut terminal = Terminal::spawn(0, 0, command, None);
        let (status, output) = terminal.finish();
        assert_eq!(status.code(), Some(1));
        assert!(contains(&output, b"doom-fire-rs:"));
        assert!(!output.contains(&0x1b));
        assert!(!contains(&output, b"panicked"));
    }
}

#[test]
fn redirected_streams_fail_without_ansi_output() {
    let (_master, slave) = pty(24, 120);
    for (terminal_input, terminal_output) in [(false, false), (true, false), (false, true)] {
        let mut command = Command::new(BINARY);
        command
            .env("COLUMNS", "120")
            .env("LINES", "24")
            .stdin(if terminal_input {
                Stdio::from(slave.try_clone().unwrap())
            } else {
                Stdio::null()
            })
            .stdout(if terminal_output {
                Stdio::from(slave.try_clone().unwrap())
            } else {
                Stdio::piped()
            })
            .stderr(Stdio::piped());
        let mut child = spawn_command(&mut command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("redirected process did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(contains(
            &result.stderr,
            b"stdin and stdout must be terminals"
        ));
        assert!(!result.stderr.contains(&0x1b));
    }
}

#[test]
fn background_start_waits_for_fg_before_drawing() {
    if !bash_available() {
        return;
    }
    let mut shell = Command::new("bash");
    shell
        .args(["--noprofile", "--norc", "-i"])
        .env("PS1", "JOBTEST> ")
        .env("HISTFILE", "/dev/null")
        .env("LC_ALL", "C");
    let mut terminal = Terminal::spawn(24, 80, shell, None);
    terminal.expect(b"JOBTEST> ");
    terminal.send(format!("'{}' &\n", BINARY.replace('\'', "'\\''")).as_bytes());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        terminal.send(b"jobs -s; printf '\\112OBSTATUS\\n'\n");
        let output = terminal.expect_until(b"JOBSTATUS\r\n", deadline);
        assert!(
            !contains(&output, ON),
            "background startup entered alternate screen"
        );
        assert!(
            !contains(&output, b"\x1b[?25l"),
            "background startup hid cursor"
        );
        assert!(
            !contains(&output, b"\x1b[2J"),
            "background startup cleared screen"
        );
        terminal.expect_until(b"JOBTEST> ", deadline);
        if contains(&output, b"Stopped") {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    terminal.send(b"fg\n");
    terminal.expect(ON);
    terminal.expect(PROMPT);
    terminal.send(b"q\n");
    assert!(contains(&terminal.expect(b"JOBTEST> "), OFF));
    terminal.send(b"exit\n");
    assert!(terminal.finish().0.success());
}

#[test]
fn sigterm_resumes_output_without_a_controlling_terminal() {
    for fire in [false, true] {
        let mut terminal = Terminal::spawn_with_session(
            24,
            if fire { 120 } else { 80 },
            Command::new(BINARY),
            None,
            false,
        );
        terminal.expect(PROMPT);
        if fire {
            terminal.send(b"\n");
            terminal.expect(FRAME_END);
        }
        terminal.send(b"\x13");
        thread::sleep(Duration::from_millis(100));
        terminal.signal(libc::SIGTERM);
        terminal.assert_restored(0);
    }
}

#[test]
fn pty_descriptors_are_not_inherited() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    const CHILD: &str = "DOOM_FIRE_TEST_FD_CHILD";
    if std::env::var_os(CHILD).is_some() {
        // Run in a fresh copy of this test executable after exec. All PTYs in
        // the parent belong to other test sessions and must have been closed.
        for fd in 3..1024 {
            assert_eq!(
                unsafe { libc::isatty(fd) },
                0,
                "inherited terminal descriptor {fd}"
            );
        }
        return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let creating = stop.clone();
    let creator = thread::spawn(move || {
        while !creating.load(Ordering::Relaxed) {
            drop(pty(24, 80));
        }
    });
    let result = (|| -> io::Result<()> {
        for _ in 0..200 {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "pty_descriptors_are_not_inherited",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = spawn_command(&mut command)?.wait_with_output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "descriptor check failed: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                )));
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    creator.join().unwrap();
    result.unwrap();
}

#[test]
fn quit_and_eof_resume_paused_output_at_each_prompt() {
    for columns in [80, 120] {
        for answer in [b"q\n".as_slice(), b"\x04"] {
            let mut terminal = Terminal::new(24, columns);
            terminal.expect(PROMPT);
            terminal.send(b"\x13");
            thread::sleep(Duration::from_millis(100));
            terminal.send(answer);
            terminal.assert_restored(0);
        }
    }
}

#[test]
fn background_start_rejects_redirected_streams_without_stopping() {
    if !bash_available() {
        return;
    }
    let mut shell = Command::new("bash");
    shell
        .args(["--noprofile", "--norc", "-i"])
        .env("PS1", "JOBTEST> ")
        .env("HISTFILE", "/dev/null")
        .env("LC_ALL", "C");
    let mut terminal = Terminal::spawn(24, 80, shell, None);
    terminal.expect(b"JOBTEST> ");
    for redirection in ["</dev/null", ">/dev/null", "</dev/null >/dev/null"] {
        terminal.send(
            format!(
                "'{}' {redirection} & job=$!; wait \"$job\"; result=$?; printf '\\105XIT:%s\\n' \"$result\"\n",
                BINARY.replace('\'', "'\\''"),
            )
            .as_bytes(),
        );
        let output = terminal.expect(b"EXIT:");
        assert!(contains(&output, b"stdin and stdout must be terminals"));
        assert!(!contains(&output, b"Stopped"));
        assert!(!contains(&output, ON));
        let status = terminal.expect(b"\r\n");
        assert_eq!(status, b"1\r\n");
        terminal.expect(b"JOBTEST> ");
    }
    terminal.send(b"exit\n");
    assert!(terminal.finish().0.success());
}
