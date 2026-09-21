use clap::Parser;

use downer::{cli::Cli, exit_code, run};

fn main() {
    // The host protocol is selected before clap sees the arguments, because
    // Firefox appends its own (the manifest path and the extension ID) to
    // whatever the launcher runs. `downer install-host` writes that launcher.
    if std::env::args().any(|argument| argument == "--native-host") {
        if let Err(error) = downer::native::run_stdio() {
            // Both, deliberately. stderr is the Browser Console, which is
            // where someone watching live would look; the log file is the only
            // one of the two that still exists tomorrow (ADR-0022). This is
            // the host's own death, so it is exactly the event the file was
            // added for.
            downer::hostlog::error("host.failed", &[("error", &error.to_string())]);
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
