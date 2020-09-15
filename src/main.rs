mod controller;
mod program;

use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let arg_len = env::args().len();
    if arg_len != 3 {
        return Err(format!("Usage: {} <file> <function>", env::args().next().unwrap()).into());
    }

    let mut args = env::args().skip(1);
    let file_path = args.next().unwrap();
    let function_name = args.next().unwrap();

    // Ensure mmap lifetime is greater than Program
    let mmap = program::mmap_file(&file_path)?;
    let program = program::Program::new(&mmap)?;
    let controller = controller::Controller::new(program, &function_name);

    Ok(())
}
