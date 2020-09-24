use crate::error::Error;
use addr2line::Location;
use object::Object;
use object::ObjectSection;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
/// Name corresponding to a function symbol that exists in the program
pub struct FunctionName(pub &'static str);

impl fmt::Display for FunctionName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.0, f)
    }
}

pub struct Program {
    /// Only used when printing error messages
    pub file_path: String,
    file: object::read::File<'static>,
    name_to_symbol: HashMap<FunctionName, SymbolInfo>,
    address_to_name: HashMap<u64, FunctionName>,
    context: addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>,
    // (start_address, size) of runtime addresses for dynamic symbols (functions
    // loaded from shared libraries)
    dynamic_symbols_range: (u64, u64),
}

#[derive(Debug)]
struct SymbolInfo {
    name: FunctionName,
    demangled_name: Option<String>,
    symbol: object::read::Symbol<'static>,
}

impl SymbolInfo {
    fn display_name(&self) -> &str {
        match &self.demangled_name {
            Some(dn) => &dn,
            None => self.name.0,
        }
    }
}

impl Program {
    pub fn new(file_path: String) -> Result<Self, Error> {
        let file = match std::fs::File::open(&file_path) {
            Ok(file) => file,
            Err(err) => return Err(format!("Failed to open file {}: {}", file_path, err).into()),
        };
        let mmap = match unsafe { memmap::Mmap::map(&file) } {
            Ok(mmap) => mmap,
            Err(err) => return Err(format!("Failed to mmap file {}: {}", file_path, err).into()),
        };
        // Yeah yeah this is a terrible thing to do. I couldn't find any way at
        // all to propagate appropriate lifetimes into cursive (if you know a
        // way let me know), so it's either making this mmap static or some
        // other struct, and doing it here simplifies LOTS of annotations.
        let mmap = Box::leak(Box::new(mmap));

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
            .filter(|(_, symbol)| symbol.kind() == object::SymbolKind::Text) // Filter to functions
            .map(|(_, symbol)| {
                symbol.name().map(|name| {
                    let demangled_name = cplus_demangle::demangle(name).ok();
                    SymbolInfo {
                        name: FunctionName(name),
                        demangled_name,
                        symbol,
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

        let address_to_name: HashMap<_, _> = name_to_symbol
            .iter()
            .filter(|(n, s)| s.symbol.address() != 0)
            .map(|(n, s)| (s.symbol.address(), n.clone()))
            .collect();

        let context = new_context(&file).unwrap();

        Ok(Program {
            file_path,
            file,
            name_to_symbol,
            address_to_name,
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
        let address = self.name_to_symbol.get(&function).unwrap().symbol.address();
        // TODO
        self.context.find_location(address).unwrap().unwrap()
    }

    pub fn get_data(&self, function: FunctionName) -> Result<Option<&[u8]>, Error> {
        let symbol = &self.name_to_symbol.get(&function).unwrap().symbol;
        if symbol.address() == 0 {
            return Err(
                format!("Cannot get data for dynamically linked symbol {}", function).into(),
            );
        }
        self.file
            .symbol_data(symbol)
            .map_err(|err| format!("Error getting data for function {}: {}", function, err).into())
    }
}

/// Clone of addr2line::ObjectContext, just using Arc instead of Rc.
///
/// Construct a new `Context`.
///
/// The resulting `Context` uses `gimli::EndianRcSlice<gimli::RunTimeEndian>`.
/// This means it is not thread safe, has no lifetime constraints (since it copies
/// the input data), and works for any endianity.
///
/// Performance sensitive applications may want to use `Context::from_sections`
/// with a more specialised `gimli::Reader` implementation.
pub fn new_context<'data, 'file, O: object::Object<'data, 'file>>(
    file: &'file O,
) -> Result<addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>, gimli::Error> {
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    fn load_section<'data, 'file, O, S, Endian>(file: &'file O, endian: Endian) -> S
    where
        O: object::Object<'data, 'file>,
        S: gimli::Section<gimli::EndianArcSlice<Endian>>,
        Endian: gimli::Endianity,
    {
        let data = file
            .section_by_name(S::section_name())
            .and_then(|section| section.uncompressed_data().ok())
            .unwrap_or(Cow::Borrowed(&[]));
        S::from(gimli::EndianArcSlice::new(Arc::from(&*data), endian))
    }

    let debug_abbrev: gimli::DebugAbbrev<_> = load_section(file, endian);
    let debug_addr: gimli::DebugAddr<_> = load_section(file, endian);
    let debug_info: gimli::DebugInfo<_> = load_section(file, endian);
    let debug_line: gimli::DebugLine<_> = load_section(file, endian);
    let debug_line_str: gimli::DebugLineStr<_> = load_section(file, endian);
    let debug_ranges: gimli::DebugRanges<_> = load_section(file, endian);
    let debug_rnglists: gimli::DebugRngLists<_> = load_section(file, endian);
    let debug_str: gimli::DebugStr<_> = load_section(file, endian);
    let debug_str_offsets: gimli::DebugStrOffsets<_> = load_section(file, endian);
    let default_section = gimli::EndianArcSlice::new(Arc::from(&[][..]), endian);

    addr2line::Context::from_sections(
        debug_abbrev,
        debug_addr,
        debug_info,
        debug_line,
        debug_line_str,
        debug_ranges,
        debug_rnglists,
        debug_str,
        debug_str_offsets,
        default_section,
    )
}
