use std::env;
use std::process::ExitCode;

use devdrop::run;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}
