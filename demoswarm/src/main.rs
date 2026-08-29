use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(demoswarm::run(std::env::args_os()))
}
