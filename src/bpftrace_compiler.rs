use itertools::Itertools;

use crate::program::FunctionName;

/// A simple AST representation of a bpftrace program which makes it a bit
/// easier to generate. Compiles to bpftrace syntax, i.e. String.
pub struct BpftraceProgram {
    blocks: Vec<Block>,
}

pub struct Block {
    block_type: BlockType,
    filter: Option<String>,
    expressions: Vec<Expression>,
}

#[derive(Copy, Clone)]
pub enum BlockType {
    Begin,
    /// Rate in seconds
    Interval {
        rate_seconds: i32,
    },
    Uprobe(FunctionName),
    UprobeOffset(FunctionName, u32),
    Uretprobe(FunctionName),
}

pub enum Expression {
    /// Expression (without terminating semicolon)
    RawExpr(String),
    If {
        condition: String,
        body: Vec<Expression>,
    },
    Printf {
        /// Will be automatically escaped when compiling
        format: String,
        args: Vec<String>,
    },
    Print(String),
}

impl BpftraceProgram {
    pub fn new() -> BpftraceProgram {
        BpftraceProgram { blocks: Vec::new() }
    }

    pub fn add(&mut self, block: Block) {
        self.blocks.push(block);
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Block> {
        self.blocks.iter_mut()
    }

    pub fn compile(&self, program_path: &str) -> String {
        // TODO add tests, show examples
        self.blocks
            .iter()
            .map(|b| b.compile(program_path))
            .join(" ")
    }
}

impl Block {
    pub fn new<T>(block_type: BlockType, filter: Option<String>, expressions: Vec<T>) -> Block
    where
        T: Into<Expression>,
    {
        Block {
            block_type,
            filter,
            expressions: expressions.into_iter().map(|e| e.into()).collect(),
        }
    }

    pub fn get_type(&self) -> BlockType {
        self.block_type
    }

    pub fn add(&mut self, expression: Expression) {
        self.expressions.push(expression);
    }

    pub fn extend<T>(&mut self, expressions: Vec<T>)
    where
        T: Into<Expression>,
    {
        self.expressions.extend(
            expressions
                .into_iter()
                .map(|e| e.into())
                .collect::<Vec<Expression>>(),
        );
    }

    pub fn compile(&self, program_path: &str) -> String {
        let mut out = String::new();
        match self.block_type {
            BlockType::Begin => out += "BEGIN",
            BlockType::Interval { rate_seconds } => out += &format!("interval:s:{}", rate_seconds),
            BlockType::Uprobe(function) => {
                out += &format!("uprobe:{}:{:?}", program_path, function)
            }
            BlockType::UprobeOffset(function, offset) => {
                out += &format!("uprobe:{}:{:?}+{}", program_path, function, offset)
            }
            BlockType::Uretprobe(function) => {
                out += &format!("uretprobe:{}:{:?}", program_path, function)
            }
        };
        if let Some(filter) = &self.filter {
            out += &format!(" /{}/", filter);
        };
        out += " { ";
        out += &Expression::compile_vec(&self.expressions);
        out += " }";
        out
    }
}

impl Expression {
    pub fn compile(&self) -> String {
        match self {
            Expression::RawExpr(ref e) => format!("{};", e),
            Expression::If {
                ref condition,
                ref body,
            } => {
                // Must not end in `;`
                format!("if ({}) {{ {} }}", condition, Expression::compile_vec(body))
            }
            Expression::Printf {
                ref format,
                ref args,
            } => {
                let args_suffix = if args.is_empty() {
                    String::new()
                } else {
                    format!(", {}", args.join(", "))
                };
                format!(
                    r#"printf("{}"{});"#,
                    format.replace('\"', r#"\""#),
                    args_suffix
                )
            }
            Expression::Print(val) => format!("print({});", val),
        }
    }

    pub fn compile_vec(expressions: &Vec<Expression>) -> String {
        expressions.iter().map(|e| e.compile()).join(" ")
    }
}

impl From<String> for Expression {
    fn from(e: String) -> Expression {
        Expression::RawExpr(e)
    }
}
impl From<&str> for Expression {
    fn from(e: &str) -> Expression {
        Expression::RawExpr(e.to_string())
    }
}
