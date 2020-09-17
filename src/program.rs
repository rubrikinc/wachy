use crate::error::Error;
use addr2line::Location;
use object::Object;
use object::ObjectSection;
use std::collections::HashMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
/// Name corresponding to a function symbol that exists in the program
pub struct FunctionName<'a>(&'a str);

pub struct Program<'a> {
    /// Only when printing error messages
    pub file_path: String,
    file: object::read::File<'a>,
    name_to_symbol: HashMap<FunctionName<'a>, SymbolInfo<'a>>,
    context: addr2line::ObjectContext,
    // (start_address, size) of runtime addresses for dynamic symbols (functions
    // loaded from shared libraries)
    dynamic_symbols_range: (u64, u64),
}

#[derive(Debug)]
struct SymbolInfo<'a> {
    name: FunctionName<'a>,
    demangled_name: Option<String>,
    address: u64,
    size: u64,
}

impl SymbolInfo<'_> {
    fn display_name(&self) -> &str {
        match &self.demangled_name {
            Some(dn) => &dn,
            None => self.name.0,
        }
    }
}

pub fn mmap_file(file_path: &str) -> Result<memmap::Mmap, Error> {
    let file = match std::fs::File::open(&file_path) {
        Ok(file) => file,
        Err(err) => return Err(format!("Failed to open file {}: {}", file_path, err).into()),
    };
    let mmap = match unsafe { memmap::Mmap::map(&file) } {
        Ok(mmap) => mmap,
        Err(err) => return Err(format!("Failed to mmap file {}: {}", file_path, err).into()),
    };
    Ok(mmap)
}

impl<'a> Program<'a> {
    pub fn new(file_path: String, mmap: &'a memmap::Mmap) -> Result<Self, Error> {
        let file = match object::File::parse(&*mmap) {
            Ok(file) => file,
            Err(err) => return Err(format!("Failed to parse file {}: {}", file_path, err).into()),
        };

        // TODO fixup unwraps
        let dynamic_symbols_range = file
            .sections()
            .filter(|s| s.name().unwrap() == ".plt")
            .map(|s| (s.address(), s.size()))
            .next()
            .unwrap();

        let function_names: Vec<SymbolInfo> = file
            .symbols()
            .filter(|(_, symbol)| symbol.kind() == object::SymbolKind::Text)
            .map(|(_, symbol)| {
                symbol.name().map(|name| {
                    let demangled_name = cplus_demangle::demangle(name).ok();
                    SymbolInfo {
                        name: FunctionName(name),
                        demangled_name,
                        address: symbol.address(),
                        size: symbol.size(),
                    }
                })
            })
            .flat_map(|x| {
                log::trace!("{:?}", x);
                x
            })
            .collect();

        // Note: reversing this map can create collisions.
        // https://stackoverflow.com/questions/49824915/ambiguity-of-de-mangled-c-symbols
        let name_to_symbol: HashMap<_, _> =
            function_names.into_iter().map(|si| (si.name, si)).collect();

        let context = addr2line::Context::new(&file).unwrap();

        Ok(Program {
            file_path,
            file,
            name_to_symbol,
            context,
            dynamic_symbols_range,
        })
    }

    pub fn get_matches(&self, function_name: &str) -> Vec<FunctionName> {
        let mut matches = Vec::new();
        for (name, symbol) in &self.name_to_symbol {
            let display_name = symbol.display_name();
            if display_name == function_name {
                return vec![*name];
            }
            if display_name.contains(function_name) {
                matches.push(*name);
            }
        }
        matches
    }

    pub fn get_location(&self, function: FunctionName) -> Location {
        let address = self.name_to_symbol.get(&function).unwrap().address;
        // TODO
        self.context.find_location(address).unwrap().unwrap()
    }
}
