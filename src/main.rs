mod controller;
mod error;
mod program;
mod tracer;
mod views;

use std::env;

fn main() {
    env_logger::init();
    let run = || -> Result<(), error::Error> {
        let arg_len = env::args().len();
        if arg_len != 3 {
            return Err(format!("Usage: {} <file> <function>", env::args().next().unwrap()).into());
        }

        let mut args = env::args().skip(1);
        let file_path = args.next().unwrap();
        let function_name = args.next().unwrap();

        let program = program::Program::new(file_path)?;
        controller::Controller::run(program, &function_name)?;
        Ok(())
    };

    let ret = run();
    if ret.is_err() {
        let err = ret.unwrap_err();
        eprintln!("Error: {}", err);
        std::process::exit(1);
    };
}
