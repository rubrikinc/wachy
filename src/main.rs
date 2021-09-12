mod controller;
mod error;
mod events;
mod program;
mod search;
mod trace_structs;
mod tracer;
mod views;

use clap::{App, Arg};
use error::Error;
use flexi_logger::{opt_format, FileSpec, Logger, LoggerHandle};
use std::env;
use std::fmt::Write;
use std::panic::PanicInfo;
use std::sync::Mutex;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

const ABOUT: &'static str = r#"A tracing profiler for arbitrary binaries using eBPF.

Keyboard shortcuts:
x - toggle tracing on current line
X - toggle tracing of an inlined function on current line
<enter> - push current call onto trace stack
> (shift+.) - specify arbitrary function to push onto trace stack
<esc> - pop function off of trace stack
r - restart trace, clear current aggregates
"#;

lazy_static::lazy_static! {
    static ref PANIC_MESSAGE: Mutex<Option<String>> = Mutex::new(None);
}

fn setup_logging() -> Result<Option<LoggerHandle>, Error> {
    if let Ok(var) = env::var("WACHY_LOG") {
        let logger = Logger::try_with_str(var)?
            .log_to_file(FileSpec::default().suppress_timestamp())
            .format(opt_format)
            .start()?;
        return Ok(Some(logger));
    }
    Ok(None)
}

fn main() {
    let _logger = setup_logging();
    let run = || -> Result<(), Error> {
        let args = App::new("wachy")
            .version(VERSION)
            .long_about(ABOUT)
            .arg(
                Arg::with_name("PROGRAM")
                    .help("Path of binary to trace")
                    .required(true),
            )
            .arg(
                Arg::with_name("FUNCTION")
                    .help("Function to trace")
                    .required(true),
            )
            .get_matches();

        // TODO make absolute
        let file_arg = args.value_of("PROGRAM").unwrap();
        let file_path = match std::fs::canonicalize(file_arg) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(err) => return Err(format!("Failed to find file {}: {}", file_arg, err).into()),
        };
        let function_name = args.value_of("FUNCTION").unwrap();

        let program = program::Program::new(file_path)?;
        controller::Controller::run(program, function_name)?;
        Ok(())
    };

    // cursive messes with terminal output so trying to print while it is still
    // displayed will not show proper output. To properly display panics, we
    // save them with a hook, drop the cursive object and then print them
    // afterwards.
    std::panic::set_hook(Box::new(|info: &PanicInfo| {
        let mut msg = String::new();
        let _ = writeln!(msg, "Panic! [v{}]", VERSION);
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
