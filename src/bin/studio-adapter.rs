use std::process::ExitCode;

fn main() -> ExitCode {
    match rusty_engine_voxels::adapter::run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
