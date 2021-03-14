use crate::error::Error;
use addr2line::fallible_iterator::FallibleIterator;
use addr2line::Location;
use object::read::File;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use object::ObjectSymbolTable;
use std::borrow::Cow;
use std::collections::{hash_map, HashMap};
use std::fmt;
use std::sync::Arc;
use zydis::ffi::Decoder;
use zydis::formatter::{Formatter, OutputBuffer};
use zydis::{
    enums::generated::{AddressWidth, FormatterStyle, MachineMode, Mnemonic},
    DecodedInstruction,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
/// Name corresponding to a function symbol that exists in the program
pub struct FunctionName(pub &'static str);

impl fmt::Display for FunctionName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.0, f)
    }
}

impl FunctionName {
    pub fn pretty_print(&self) -> String {
        cplus_demangle::demangle(self.0).unwrap_or(String::from(self.0))
    }
}

pub struct Program {
    /// Only used when printing error messages
    pub file_path: String,
    file: File<'static>,
    name_to_symbol: Arc<HashMap<FunctionName, SymbolInfo>>,
    address_to_name: HashMap<u64, FunctionName>,
    context: addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>,
    // (start_address, size) of runtime addresses for dynamic symbols (functions
    // loaded from shared libraries)
    dynamic_symbols_ranges: Vec<std::ops::Range<u64>>,
    dynamic_symbols_map: HashMap<u64, FunctionName>,
}

pub struct SymbolsGenerator {
    name_to_symbol: Arc<HashMap<FunctionName, SymbolInfo>>,
}

impl<'a> IntoIterator for &'a SymbolsGenerator {
    type Item = &'a SymbolInfo;
    type IntoIter = hash_map::Values<'a, FunctionName, SymbolInfo>;
    fn into_iter(self) -> Self::IntoIter {
        self.name_to_symbol.values()
    }
}

#[derive(Clone, Debug)]
pub struct SymbolInfo {
    pub name: FunctionName,
    demangled_name: Option<String>,
    section_index: Option<object::SectionIndex>,
    address: u64,
    size: u64,
}

impl AsRef<str> for SymbolInfo {
    fn as_ref(&self) -> &str {
        match &self.demangled_name {
            Some(dn) => &dn,
            None => self.name.0,
        }
    }
}

impl fmt::Display for SymbolInfo {
    // This is used to display the symbol in search results
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.address == 0 {
            // Undefined symbol
            fmt::Display::fmt("(D) ", f)?
        }
        fmt::Display::fmt(self.as_ref(), f)
    }
}

fn should_log_verbose() -> bool {
    std::env::var("WACHY_PROGRAM_TRACE").unwrap_or(String::new()) == "1"
}

impl Program {
    pub fn new(file_path: String) -> Result<Self, Error> {
        let file = Program::parse(&file_path)?;

        // TODO fixup unwraps
        let dynamic_symbols_ranges = file
            .sections()
            .filter(|s| s.name().unwrap().starts_with(".plt")) // Include .plt and .plt.got
            .map(|s| std::ops::Range {
                start: s.address(),
                end: s.address() + s.size(),
            })
            .collect();

        let mut versioned_symbols_map: HashMap<String, FunctionName> = HashMap::new();
        let symbols: Vec<SymbolInfo> = file
            .symbols()
            .filter(|symbol| symbol.kind() == object::SymbolKind::Text) // Filter to functions
            .map(|symbol| {
                symbol.name().map(|name| {
                    let demangled_name = cplus_demangle::demangle(name).ok();
                    let function = FunctionName(name);
                    if name.contains("@@") {
                        versioned_symbols_map
                            .insert(name.split("@@").next().unwrap().to_string(), function);
                    }
                    SymbolInfo {
                        name: function,
                        demangled_name,
                        section_index: symbol.section_index(),
                        address: symbol.address(),
                        size: symbol.size(),
                    }
                })
            })
            .flat_map(|x| {
                if should_log_verbose() {
                    log::trace!("{:?}", x);
                }
                x
            })
            .collect();

        let dynamic_symbols_map = Program::dynamic_symbols_map(&file, &versioned_symbols_map);

        let name_to_symbol: HashMap<_, _> = symbols.into_iter().map(|si| (si.name, si)).collect();

        let address_to_name: HashMap<_, _> = name_to_symbol
            .iter()
            .filter(|(_, s)| s.address != 0)
            .map(|(n, s)| (s.address, n.clone()))
            .collect();

        // Try to find file containing `.debug_line` section - if it's not in
        // the passed in binary, check debuglink.
        let debug_file;
        let debug_file_ref = match file.section_by_name(".debug_line") {
            Some(_) => &file,
            None => match Program::get_debug_file(&file) {
                Ok(df) => {
                    debug_file = df;
                    &debug_file
                }
                Err(err) => {
                    log::info!(
                        "Failed to get debug file: {}, falling back to original file {}",
                        err,
                        file_path
                    );
                    &file
                }
            },
        };
        let context = new_context(debug_file_ref).unwrap();

        Ok(Program {
            file_path,
            file,
            name_to_symbol: Arc::new(name_to_symbol),
            address_to_name,
            context,
            dynamic_symbols_ranges,
            dynamic_symbols_map,
        })
    }

    fn parse(file_path: &String) -> Result<File<'static>, Error> {
        let file = match std::fs::File::open(&file_path) {
            Ok(file) => file,
            Err(err) => return Err(format!("Failed to open file {}: {}", file_path, err).into()),
        };
        let mmap = match unsafe { memmap::Mmap::map(&file) } {
            Ok(mmap) => mmap,
            Err(err) => return Err(format!("Failed to mmap file {}: {}", file_path, err).into()),
        };
        // Yeah yeah this is a terrible thing to do. I couldn't find any way to
        // propagate appropriate lifetimes into cursive, so it's either making
        // this mmap static or some other struct, and doing it here simplifies
        // LOTS of annotations.
        let mmap = Box::leak(Box::new(mmap));

        match object::File::parse(&*mmap) {
            Ok(file) => Ok(file),
            Err(err) => return Err(format!("Failed to parse file {}: {}", file_path, err).into()),
        }
    }

    // `versioned_symbols_map` is a map from unversioned symbol name to the
    // versioned one. The dynamic symbols section seems to contain unversioned
    // symbol names.
    fn dynamic_symbols_map(
        file: &File<'static>,
        versioned_symbols_map: &HashMap<String, FunctionName>,
    ) -> HashMap<u64, FunctionName> {
        let mut relocations = HashMap::new();
        let dynamic_symbols = file.dynamic_symbol_table().unwrap();
        let reloc_iter = file.dynamic_relocations().unwrap();
        for (address, relocation) in reloc_iter {
            if let object::RelocationTarget::Symbol(index) = relocation.target() {
                let symbol = dynamic_symbols.symbol_by_index(index).unwrap();
                if symbol.kind() == object::SymbolKind::Text {
                    if let Ok(name) = symbol.name() {
                        if should_log_verbose() {
                            log::trace!("Relocation {:x} = {}", address, name);
                        }
                        relocations.insert(address, name);
                    }
                }
            }
        }

        let mut map = HashMap::new();
        let decoder = create_decoder();
        for section in file.sections() {
            if let (Ok(name), address) = (section.name(), section.address()) {
                // Include .plt and .plt.got
                if name.starts_with(".plt") {
                    let code = section.uncompressed_data().unwrap();
                    for (instruction, ip) in
                        get_instructions_with_mnemonic(&decoder, address, &code, Mnemonic::JMP)
                    {
                        assert!(instruction.operand_count > 0);
                        let jump_address = instruction
                            .calc_absolute_address(ip, &instruction.operands[0])
                            .unwrap();
                        if should_log_verbose() {
                            log::trace!("PLT {:#x?} -> GOT {:#x?}", ip, jump_address);
                        }
                        // Ignore expected jumps to PLT0 - figure A-9 in
                        // https://refspecs.linuxfoundation.org/elf/elf.pdf
                        if let Some(&name) = relocations.get(&jump_address) {
                            let name = if let Some(versioned_name) = versioned_symbols_map.get(name)
                            {
                                *versioned_name
                            } else {
                                FunctionName(name)
                            };
                            map.insert(ip, name);
                        }
                    }
                }
            }
        }
        log::trace!("{:?}", map);
        map
    }

    fn get_debug_file(program_file: &File<'static>) -> Result<File<'static>, Error> {
        let debug_link = match program_file.gnu_debuglink() {
            Ok(link_opt) => match link_opt {
                Some(link) => {
                    // FIXME: we should validate checksum
                    std::str::from_utf8(link.0).unwrap().to_string()
                }
                None => return Err("No debuglink found".into()),
            },
            Err(err) => return Err(format!("Failed to get debuglink: {}", err).into()),
        };
        // TODO if file doesn't exist in cwd we should probably check in
        // original file_path's folder.
        let df = Program::parse(&debug_link);
        if df.is_ok() {
            log::info!(
                "Using debuglink file {} for address to line mappings",
                debug_link
            );
        }
        df
    }

    pub fn get_matches(&self, function_name: &str) -> Vec<FunctionName> {
        let mut matches = Vec::new();
        for (name, symbol) in &*self.name_to_symbol {
            let display_name = symbol.as_ref();
            if display_name == function_name {
                return vec![*name];
            }
            if display_name.contains(function_name) {
                matches.push(*name);
            }
        }
        log::debug!("Matches for {}: {:?}", function_name, matches);
        matches
    }

    pub fn get_address(&self, function: FunctionName) -> u64 {
        self.name_to_symbol.get(&function).unwrap().address
    }

    /// If something is returned, it is guaranteed to have file and line number
    /// set.
    pub fn get_location(&self, address: u64) -> Option<Location> {
        match self.context.find_location(address) {
            Ok(l) => match l {
                Some(l) => {
                    l.file?;
                    l.line?;
                    Some(l)
                }
                None => None,
            },
            Err(_) => None,
        }
    }

    #[allow(dead_code)]
    fn print_frames(&self, address: u64) {
        log::info!(
            "{:#?}",
            self.context
                .find_frames(address)
                .unwrap()
                .collect::<Vec<addr2line::Frame<_>>>()
                .unwrap()
                .iter()
                .map(|f| f.location.as_ref().unwrap().file)
                .collect::<Vec<_>>()
        );
    }

    // Returns (address, data) for given function
    pub fn get_data(&self, function: FunctionName) -> Result<(u64, &[u8]), Error> {
        let symbol = &self.name_to_symbol.get(&function).unwrap();
        let address = symbol.address;
        if address == 0 {
            return Err(
                format!("Cannot get data for dynamically linked symbol {}", function).into(),
            );
        }
        let size = symbol.size;
        let index = symbol.section_index.unwrap();
        Ok((
            address,
            self.file
                .section_by_index(index)
                .unwrap()
                .data_range(address, size)
                .unwrap()
                .unwrap(),
        ))
    }

    pub fn get_symbol(&self, function: FunctionName) -> &SymbolInfo {
        &self.name_to_symbol.get(&function).unwrap()
    }

    pub fn symbols_generator(&self) -> SymbolsGenerator {
        SymbolsGenerator {
            name_to_symbol: Arc::clone(&self.name_to_symbol),
        }
    }

    pub fn get_function_for_address(&self, address: u64) -> Option<FunctionName> {
        if self.is_dynamic_symbol_address(address) {
            self.dynamic_symbols_map.get(&address).map(|f| f.clone())
        } else {
            self.address_to_name.get(&address).map(|f| f.clone())
        }
    }

    pub fn is_dynamic_symbol_address(&self, address: u64) -> bool {
        self.dynamic_symbols_ranges
            .iter()
            .any(|r| r.contains(&address))
    }

    pub fn is_dynamic_symbol(&self, symbol: &SymbolInfo) -> bool {
        self.is_dynamic_symbol_address(symbol.address)
    }
}

pub fn create_decoder() -> Decoder {
    // TODO make platform independent
    Decoder::new(MachineMode::LONG_64, AddressWidth::_64).unwrap()
}

pub fn get_instructions_with_mnemonic<'a, 'b>(
    decoder: &'a Decoder,
    start_address: u64,
    code: &'b [u8],
    mnemonic: Mnemonic,
) -> CallIterator<'a, 'b> {
    CallIterator {
        it: decoder.instruction_iterator(code, start_address),
        mnemonic,
    }
}

pub struct CallIterator<'a, 'b> {
    it: zydis::InstructionIterator<'a, 'b>,
    mnemonic: Mnemonic,
}

impl Iterator for CallIterator<'_, '_> {
    type Item = (DecodedInstruction, u64);

    fn next(&mut self) -> Option<(DecodedInstruction, u64)> {
        while let Some((instruction, ip)) = self.it.next() {
            if instruction.mnemonic == self.mnemonic {
                if log::log_enabled!(log::Level::Trace) {
                    let formatter = Formatter::new(FormatterStyle::INTEL).unwrap();
                    let mut buffer = [0u8; 200];
                    let mut buffer = OutputBuffer::new(&mut buffer[..]);
                    formatter
                        .format_instruction(&instruction, &mut buffer, Some(ip), None)
                        .unwrap();
                    log::trace!("{} 0x{:016X} {}", instruction.operand_count, ip, buffer);
                }

                return Some((instruction, ip));
            }
        }
        None
    }
}

/// Clone of addr2line::ObjectContext::new, just using Arc instead of Rc.
///
/// Construct a new `Context`.
///
/// The resulting `Context` uses `gimli::EndianRcSlice<gimli::RunTimeEndian>`.
/// This means it is not thread safe, has no lifetime constraints (since it copies
/// the input data), and works for any endianity.
///
/// Performance sensitive applications may want to use `Context::from_sections`
/// with a more specialised `gimli::Reader` implementation.
pub fn new_context<'data: 'file, 'file, O: object::Object<'data, 'file>>(
    file: &'file O,
) -> Result<addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>, gimli::Error> {
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    fn load_section<'data: 'file, 'file, O, S, Endian>(file: &'file O, endian: Endian) -> S
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
