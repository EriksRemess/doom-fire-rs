mod app;
mod cli;
mod fire;
mod frame;
mod kitty;
mod platform;
mod rng;
mod terminal;

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(err) = run() {
        eprintln!("doom-fire-rs: {err}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    match cli::parse(std::env::args_os().skip(1))? {
        cli::Action::Run(output) => app::run(output)?,
        cli::Action::Help => println!("{}", cli::HELP),
        cli::Action::Version => println!("doom-fire-rs {}", env!("CARGO_PKG_VERSION")),
    }
    Ok(())
}
