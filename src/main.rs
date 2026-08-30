//! `ticket-tui`: a terminal browser for Azure DevOps work items.

mod run;

fn main() {
    if let Err(error) = run::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
