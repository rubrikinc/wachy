use crate::program::Program;

pub struct Controller<'a> {
    program: Program<'a>,
}

impl<'a> Controller<'a> {
    pub fn new(program: Program<'a>, function_name: &str) -> Controller<'a> {
        let matches = program.get_matches(function_name);
        // TODO ensure one and only one match
        let function = matches.into_iter().next().unwrap();
        let location = program.get_location(function);
        println!("{} {}", location.file.unwrap(), location.line.unwrap());
        Controller { program }
    }
}
