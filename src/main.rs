use clap::Parser;

use downer::{cli::Cli, exit_code, run};

fn main() {
    if std::env::args().any(|argument| argument == "--native-host") {
        if let Err(error) = downer::native::run_stdio() {
            eprintln!("native host error: {error}");
            std::process::exit(1);
        }
        return;
    }

    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        eprintln!("error: {error}");
        std::process::exit(exit_code(&error));
    }
}
