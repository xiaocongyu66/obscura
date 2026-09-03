//! GLSL ES 1.0 compiler and interpreter.
//!
//! Uses the `glsl` crate (pure Rust GLSL450/460 parser) to parse GLSL
//! source into an AST, then compiles the AST into bytecode that the
//! interpreter runs at draw time.
//!
//! Layout:
//! - [`ast`] — our own AST (translated from `glsl` crate's AST)
//! - [`compiler`] — AST → bytecode
//! - [`opcode`] — bytecode opcode definitions
//! - [`interpreter`] — bytecode interpreter (runs per vertex / per fragment)
//! - [`trampoline`] — `extern "C"` trampolines that PortableGL calls

pub mod ast;
pub mod compiler;
pub mod interpreter;
pub mod opcode;
pub mod parser;
pub mod trampoline;

pub use compiler::compile_shader;
pub use interpreter::Interpreter;
pub use parser::parse_glsl;
