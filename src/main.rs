mod controller;
mod error;
mod events;
mod program;
mod trace_structs;
mod tracer;
mod views;

use std::env;
use std::fmt::Write;
use std::panic::PanicInfo;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref PANIC_MESSAGE: Mutex<Option<String>> = Mutex::new(None);
}

fn setup_logging() {
    if let Ok(var) = env::var("WACHY_LOG") {
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
        // TODO make absolute
        let file_path = args.next().unwrap();
        let function_name = args.next().unwrap();

        let program = program::Program::new(file_path)?;
        controller::Controller::run(program, &function_name)?;
        Ok(())
    };

    // cursive messes with terminal output so trying to print while it is still
    // displayed will not show proper output. To properly display panics, we
    // save them with a hook, drop the cursive object and then print them
    // afterwards.
    std::panic::set_hook(Box::new(|info: &PanicInfo| {
        let mut msg = String::new();
        let _ = writeln!(msg, "Panic! [v{}]", env!("CARGO_PKG_VERSION"));
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            let _ = writeln!(msg, "Cause: {}", payload);
        }

        if let Some(location) = info.location() {
            let _ = writeln!(msg, "Location: {}.", location);
        }

        let _ = writeln!(msg);
        let _ = writeln!(msg, "{:#?}", backtrace::Backtrace::new());

        log::error!("{}", msg);

        let mut saved_panic = PANIC_MESSAGE.lock().unwrap();
        // Only store first backtrace
        if saved_panic.is_none() {
            *saved_panic = Some(msg)
        }
    }));

    // catch_unwind doesn't give us stacktrace, that's why we use a panic hook
    // too.
    let ret = std::panic::catch_unwind(|| run());
    if let Some(msg) = PANIC_MESSAGE.lock().unwrap().clone() {
        log::error!("{}", msg);
        eprintln!("Error: {}", msg);
        std::process::exit(1);
    }
    if let Ok(Err(err)) = ret {
        log::error!("{}", err);
        eprintln!("Error: {}", err);
        std::process::exit(1);
    };
}
