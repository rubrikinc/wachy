mod controller;
mod error;
mod program;
mod tracer;
mod views;

use std::env;
use std::fmt::Write;
use std::panic::PanicInfo;

fn setup_logging() {
    if let Ok(var) = env::var("RUST_LOG") {
        let filter = match &var[..] {
            "error" => Some(log::LevelFilter::Error),
            "warn" => Some(log::LevelFilter::Warn),
            "info" => Some(log::LevelFilter::Info),
            "debug" => Some(log::LevelFilter::Debug),
            "trace" => Some(log::LevelFilter::Trace),
            _ => None,
        };
        filter.map(|f| simple_logging::log_to_file("wachy.log", f));
    }
}

fn main() {
    setup_logging();
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

    std::panic::set_hook(Box::new(|info: &PanicInfo| {
        // TODO write to separate log file, print after dropping cursive
        let mut msg = String::new();
        let _ = writeln!(msg, "Panic! [v{}].", env!("CARGO_PKG_VERSION"));
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            let _ = writeln!(msg, "Cause: {}", payload);
        }

        if let Some(location) = info.location() {
            let _ = writeln!(msg, "Location: {}.", location);
        }

        let _ = writeln!(msg);
        let _ = writeln!(msg, "{:#?}", backtrace::Backtrace::new());

        log::error!("{}", msg);
        eprintln!("{}", msg);
    }));

    let ret = run();
    if ret.is_err() {
        let err = ret.unwrap_err();
        eprintln!("Error: {}", err);
        std::process::exit(1);
    };
}
