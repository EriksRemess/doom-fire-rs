use std::{
    fmt,
    io::{self, BufRead, IsTerminal, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    AppResult,
    cli::{Options, Output},
    fire::Fire,
    frame::{FrameBuffer, FrameTiming},
    kitty,
    platform::{self, interrupted},
    rng::Rng,
    terminal::*,
};

mod preview;

#[derive(Clone, Copy)]
enum PauseScreen {
    SizeWarning,
    Capabilities,
}

struct App {
    stdout: io::Stdout,
    console: platform::Console,
    keyboard: Option<platform::Keyboard>,
    term_sz: TermSize,
    fg: Vec<String>,
    bg: Vec<String>,
    rng: Rng,
    terminal_active: bool,
    output: Output,
    kitty_scale: usize,
}

impl App {
    fn new(options: Options) -> AppResult<Self> {
        let fg = init_colors("38;5;");
        let bg = init_colors("48;5;");
        let console = platform::init_console()?;
        let term_sz = platform::term_size(&console)?.validate()?;
        let rng = Rng::seeded();

        let mut app = Self {
            stdout: io::stdout(),
            console,
            keyboard: None,
            term_sz,
            fg,
            bg,
            rng,
            terminal_active: true,
            output: options.output,
            kitty_scale: options.kitty_scale,
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
        let keyboard_result = self
            .keyboard
            .take()
            .map_or(Ok(()), platform::Keyboard::finish);
        if self.terminal_active {
            #[cfg(unix)]
            platform::resume_output();
            if self.output == Output::Kitty {
                self.emit(kitty::DELETE_IMAGE)?;
            }
            self.emit(&term_off())?;
            self.stdout.flush()?;
            self.terminal_active = false;
        }
        keyboard_result
            .map_err(|error| io::Error::new(error.kind(), format!("keyboard input: {error}")))
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
                    platform::request_exit();
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

    fn handle_suspend(&mut self) -> AppResult<Option<Duration>> {
        #[cfg(unix)]
        if !interrupted() && platform::take_suspend_request() {
            let start = Instant::now();
            let animation = self.keyboard.is_some();
            self.complete()?;
            platform::suspend_process()?;
            if !interrupted() {
                self.term_sz = platform::term_size(&self.console)?.validate()?;
                self.terminal_active = true;
                self.emit(&term_on())?;
                self.stdout.flush()?;
                if animation {
                    self.keyboard = Some(platform::Keyboard::start()?);
                }
            }
            return Ok(Some(start.elapsed()));
        }
        Ok(None)
    }

    fn new_fire(&self, size: TermSize) -> AppResult<Fire> {
        match self.output {
            Output::Ansi => Fire::new(size),
            Output::Kitty => Fire::new_scaled(size, self.kitty_scale),
        }
    }

    fn show_doom_fire(&mut self) -> AppResult<()> {
        self.term_sz = platform::term_size(&self.console)?.validate()?;
        let mut fire = self.new_fire(self.term_sz)?;
        let mut frame =
            FrameBuffer::new_for_fire(self.term_sz, &self.fg, &self.bg, self.output, &fire)?;
        self.emit(SCREEN_CLEAR)?;
        self.keyboard = Some(platform::Keyboard::start()?);

        while !interrupted() {
            if let Some(paused) = self.handle_suspend()? {
                frame.exclude_pause(paused);
            }
            if interrupted() {
                break;
            }
            let size = platform::term_size(&self.console)?.validate()?;
            if size != frame.size() {
                fire = self.new_fire(size)?;
                frame = FrameBuffer::new_for_fire(size, &self.fg, &self.bg, self.output, &fire)?;
                self.term_sz = size;
                self.emit(SCREEN_CLEAR)?;
            }

            let start = Instant::now();
            fire.advance(&mut self.rng);
            let simulation = start.elapsed();
            let start = Instant::now();
            frame.draw_fire(&fire, &self.fg, &self.bg);
            let bytes = frame.finish_frame();
            let encoding = start.elapsed();
            let start = Instant::now();
            self.emit_bytes(bytes)?;
            self.stdout.flush()?;
            frame.complete_frame(FrameTiming {
                simulation,
                encoding,
                write: start.elapsed(),
            });
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

pub(crate) fn run(options: Options) -> AppResult<()> {
    // Reject invalid streams before job control can suspend a background launch.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("stdin and stdout must be terminals".into());
    }
    platform::reset_interrupt();
    platform::install_exit_handlers()?;

    #[cfg(unix)]
    platform::wait_for_foreground()?;
    if interrupted() {
        return Ok(());
    }

    let mut app = App::new(options)?;
    let result = app.run();
    let cleanup = app.complete();

    result?;
    cleanup?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
