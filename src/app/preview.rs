use std::{
    thread,
    time::{Duration, Instant},
};

use super::{App, PauseScreen};
use crate::{AppResult, cli::Output, platform::interrupted, terminal::*};

impl App {
    fn preview_gap(&mut self) -> AppResult<()> {
        self.emit(COLOR_DEF)?;
        if self.term_sz.height >= 28 {
            self.emit(nl())?;
        }
        Ok(())
    }

    pub(super) fn check_term_size(&mut self) -> AppResult<()> {
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

    pub(super) fn show_size_warning(&mut self) -> AppResult<()> {
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
        let (renderer, scale) = match self.output {
            Output::Ansi => ("ANSI half-block", 1),
            Output::Kitty => ("Kitty RGB", self.kitty_scale),
        };
        self.emit_fmt(format_args!(
            "Output: {renderer} | res: {}x{} | animation exit: q / Ctrl-C",
            width * scale,
            (height - 1) * 2 * scale
        ))?;
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
        self.show_extended_capabilities()?;
        Ok(())
    }

    fn show_extended_capabilities(&mut self) -> AppResult<()> {
        self.preview_gap()?;
        self.emit("RGB: ")?;
        let width = self.term_sz.width.saturating_sub(6).min(103);
        for x in 0..width {
            let red = x * 255 / width.saturating_sub(1).max(1);
            let blue = 255 - red;
            let green = 255 - red.abs_diff(128).min(127) * 2;
            self.emit_fmt(format_args!("{CSI}48;2;{red};{green};{blue}m "))?;
        }
        self.emit(COLOR_DEF)?;
        self.emit(nl())?;
        self.preview_gap()?;
        self.emit(concat!(
            "Text: \x1b[1mbold\x1b[22m \x1b[2mdim\x1b[22m ",
            "\x1b[3mitalic\x1b[23m \x1b[4munderline\x1b[24m ",
            "\x1b[7mreverse\x1b[27m \x1b[9mstrike\x1b[29m | Blocks: ▀▄█ ░▒▓"
        ))?;
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

    pub(super) fn show_capability_colors(&mut self) -> AppResult<()> {
        self.show_term_size()?;
        self.preview_gap()?;
        self.show_standard_colors()?;
        self.preview_gap()?;
        self.show_216_colors()?;
        self.show_grayscale()?;
        self.preview_gap()
    }

    pub(super) fn show_term_capabilities(&mut self) -> AppResult<()> {
        self.show_capability_colors()?;
        self.scroll_marquee()?;
        if interrupted() {
            Ok(())
        } else {
            self.preview_gap()?;
            self.pause(PauseScreen::Capabilities)
        }
    }
}
