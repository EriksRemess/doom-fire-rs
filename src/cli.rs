use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    Ansi,
    Kitty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Options {
    pub(crate) output: Output,
    pub(crate) kitty_scale: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Run(Options),
    Help,
    Version,
}

pub(crate) const HELP: &str = "DOOM fire terminal benchmark

Usage: doom-fire-rs [--kitty [--kitty-scale N]]

Options:
  --kitty       Stream RGB frames using the Kitty graphics protocol
  --kitty-scale N  Pixels per simulation axis relative to ANSI (1..16; default 4)
  -h, --help    Show this help
  -V, --version Show the version

The default output uses ANSI half-block characters.
Stop the benchmark with q or Ctrl-C.";

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut output = Output::Ansi;
    let mut scale = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--kitty") => output = Output::Kitty,
            Some("--kitty-scale") => {
                let value = args
                    .next()
                    .ok_or("--kitty-scale requires an integer from 1 to 16")?;
                let value = value
                    .to_str()
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|n| (1..=16).contains(n))
                    .ok_or("--kitty-scale requires an integer from 1 to 16")?;
                scale = Some(value);
            }
            Some("-h" | "--help") => return Ok(Action::Help),
            Some("-V" | "--version") => return Ok(Action::Version),
            _ => {
                return Err(format!(
                    "unknown argument: {} (see --help)",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    if scale.is_some() && output != Output::Kitty {
        return Err("--kitty-scale requires --kitty".into());
    }
    Ok(Action::Run(Options {
        output,
        kitty_scale: scale.unwrap_or(4),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_and_informational_options() {
        for (args, expected) in [
            (
                vec![],
                Action::Run(Options {
                    output: Output::Ansi,
                    kitty_scale: 4,
                }),
            ),
            (
                vec!["--kitty"],
                Action::Run(Options {
                    output: Output::Kitty,
                    kitty_scale: 4,
                }),
            ),
            (vec!["--help"], Action::Help),
            (vec!["--version"], Action::Version),
        ] {
            assert_eq!(
                parse(args.into_iter().map(OsString::from)).unwrap(),
                expected
            );
        }
        assert!(parse([OsString::from("--unknown")]).is_err());
    }

    #[test]
    fn validates_kitty_scale() {
        let parse_args = |args: &[&str]| parse(args.iter().map(OsString::from));
        assert_eq!(
            parse_args(&["--kitty", "--kitty-scale", "8"]).unwrap(),
            Action::Run(Options {
                output: Output::Kitty,
                kitty_scale: 8
            })
        );
        for args in [
            vec!["--kitty-scale", "4"],
            vec!["--kitty", "--kitty-scale"],
            vec!["--kitty", "--kitty-scale", "0"],
            vec!["--kitty", "--kitty-scale", "17"],
            vec!["--kitty", "--kitty-scale", "no"],
        ] {
            assert!(parse_args(&args).is_err());
        }
    }
}
