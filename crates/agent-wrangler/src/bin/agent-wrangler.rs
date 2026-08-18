//! The client as a console program. What a subcommand prints arrives in the
//! shell that asked for it.

fn main() -> std::process::ExitCode {
    agent_wrangler::run()
}
