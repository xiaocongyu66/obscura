//! Bytecode interpreter for GLSL.
//!
//! Runs the bytecode produced by [`crate::glsl::compiler`]. The interpreter
//! is a stack machine: each instruction pops operands, computes, and
//! pushes the result.
//!
//! Values on the stack are `f32` — vectors and matrices are represented
//! as multiple consecutive stack slots (vec4 = 4 slots). This is simple
//! but slow; a real implementation would use typed registers.

use crate::glsl::opcode::*;
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

/// Thread-local pointers to the currently-running vertex and fragment
/// interpreters. The trampolines (extern "C") use these to find the
/// interpreter instances that PortableGL can't pass directly.
thread_local! {
    pub static CURRENT_VERTEX_INTERPRETER: RefCell<Option<*mut Interpreter>> = RefCell::new(None);
    pub static CURRENT_FRAGMENT_INTERPRETER: RefCell<Option<*mut Interpreter>> = RefCell::new(None);
}

pub struct Interpreter {
    pub program: ShaderProgram,
    pub stack: Vec<f32>,
    pub variables: Vec<f32>,    // local + global variables
    pub uniforms: Vec<f32>,
    pub attribs: Vec<f32>,
    pub varyings: Vec<f32>,
    pub builtins: Vec<f32>,     // gl_Position (4), gl_FragColor (4), etc.
}

impl Interpreter {
    pub fn new(program: ShaderProgram) -> Self {
        Self {
            program,
            stack: Vec::with_capacity(256),
            variables: vec![0.0; 256],
            uniforms: vec![0.0; 256],
            attribs: vec![0.0; 256],
            varyings: vec![0.0; 256],
            builtins: vec![0.0; 32],
        }
    }

    /// Run the shader (main function). After this, builtins (gl_Position,
    /// gl_FragColor) are populated.
    pub fn run(&mut self) {
        self.stack.clear();
        let mut ip = self.program.entry as usize;
        let n = self.program.instructions.len();
        while ip < n {
            let inst = self.program.instructions[ip].clone();
            ip += 1;
            match inst.op {
                Op::PushFloat => {
                    let v = self.program.float_consts.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::PushInt => {
                    let v = self.program.int_consts.get(inst.arg as usize).copied().unwrap_or(0);
                    self.stack.push(v as f32);
                }
                Op::PushBool => self.stack.push(if inst.arg != 0 { 1.0 } else { 0.0 }),
                Op::Pop => { self.stack.pop(); }
                Op::Load => {
                    let v = self.variables.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::Store => {
                    if let Some(v) = self.stack.pop() {
                        if let Some(slot) = self.variables.get_mut(inst.arg as usize) {
                            *slot = v;
                        }
                    }
                }
                Op::LoadAttrib => {
                    let v = self.attribs.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::LoadUniform => {
                    let v = self.uniforms.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::LoadVarying => {
                    let v = self.varyings.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::LoadBuiltin => {
                    let v = self.builtins.get(inst.arg as usize).copied().unwrap_or(0.0);
                    self.stack.push(v);
                }
                Op::StoreVarying => {
                    if let Some(v) = self.stack.pop() {
                        if let Some(slot) = self.varyings.get_mut(inst.arg as usize) {
                            *slot = v;
                        }
                    }
                }
                Op::StoreBuiltin => {
                    if let Some(v) = self.stack.pop() {
                        if let Some(slot) = self.builtins.get_mut(inst.arg as usize) {
                            *slot = v;
                        }
                    }
                }
                Op::Add => self.binary(|a, b| a + b),
                Op::Sub => self.binary(|a, b| a - b),
                Op::Mul => self.binary(|a, b| a * b),
                Op::Div => self.binary(|a, b| if b != 0.0 { a / b } else { 0.0 }),
                Op::Mod => self.binary(|a, b| a % b),
                Op::Equal => self.binary(|a, b| if a == b { 1.0 } else { 0.0 }),
                Op::NotEqual => self.binary(|a, b| if a != b { 1.0 } else { 0.0 }),
                Op::Less => self.binary(|a, b| if a < b { 1.0 } else { 0.0 }),
                Op::Greater => self.binary(|a, b| if a > b { 1.0 } else { 0.0 }),
                Op::LessEq => self.binary(|a, b| if a <= b { 1.0 } else { 0.0 }),
                Op::GreaterEq => self.binary(|a, b| if a >= b { 1.0 } else { 0.0 }),
                Op::And => self.binary(|a, b| if a != 0.0 && b != 0.0 { 1.0 } else { 0.0 }),
                Op::Or => self.binary(|a, b| if a != 0.0 || b != 0.0 { 1.0 } else { 0.0 }),
                Op::Not => self.unary(|a| if a == 0.0 { 1.0 } else { 0.0 }),
                Op::Neg => self.unary(|a| -a),
                Op::Abs => self.unary(|a| a.abs()),
                Op::Sign => self.unary(|a| a.signum()),
                Op::Floor => self.unary(|a| a.floor()),
                Op::Ceil => self.unary(|a| a.ceil()),
                Op::Fract => self.unary(|a| a.fract()),
                Op::Sin => self.unary(|a| a.sin()),
                Op::Cos => self.unary(|a| a.cos()),
                Op::Tan => self.unary(|a| a.tan()),
                Op::Asin => self.unary(|a| a.asin()),
                Op::Acos => self.unary(|a| a.acos()),
                Op::Atan => self.unary(|a| a.atan()),
                Op::Pow => self.binary(|a, b| a.powf(b)),
                Op::Exp => self.unary(|a| a.exp()),
                Op::Log => self.unary(|a| a.ln()),
                Op::Exp2 => self.unary(|a| a.exp2()),
                Op::Log2 => self.unary(|a| a.log2()),
                Op::Sqrt => self.unary(|a| a.sqrt()),
                Op::Min => self.binary(|a, b| a.min(b)),
                Op::Max => self.binary(|a, b| a.max(b)),
                Op::Mix => {
                    let c = self.pop();
                    let b = self.pop();
                    let a = self.pop();
                    self.stack.push(a + (b - a) * c);
                }
                Op::Clamp => {
                    let max = self.pop();
                    let min = self.pop();
                    let v = self.pop();
                    self.stack.push(v.max(min).min(max));
                }
                Op::Step => {
                    let v = self.pop();
                    let edge = self.pop();
                    self.stack.push(if v < edge { 0.0 } else { 1.0 });
                }
                Op::SmoothStep => {
                    let v = self.pop();
                    let edge1 = self.pop();
                    let edge0 = self.pop();
                    let t = ((v - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
                    self.stack.push(t * t * (3.0 - 2.0 * t));
                }
                Op::Length => self.unary(|a| a.abs()),
                Op::Distance => {
                    let b = self.pop();
                    let a = self.pop();
                    self.stack.push((a - b).abs());
                }
                Op::Normalize => self.unary(|a| if a != 0.0 { a / a.abs() } else { 0.0 }),
                Op::Dot => self.binary(|a, b| a * b),
                Op::Cross => { /* simplified for scalars */ self.binary(|a, b| a * b); }
                Op::Reflect => {
                    let n = self.pop();
                    let i = self.pop();
                    self.stack.push(i - 2.0 * (i * n) * n);
                }
                Op::Refract => { self.stack.pop(); self.stack.pop(); self.stack.pop(); self.stack.push(0.0); }
                Op::FaceForward => { self.stack.pop(); self.stack.pop(); self.stack.pop(); self.stack.push(0.0); }
                Op::Texture2D => { self.stack.pop(); self.stack.pop(); self.stack.push(0.0); self.stack.push(0.0); self.stack.push(0.0); self.stack.push(1.0); }
                Op::TextureCube => { self.stack.pop(); self.stack.pop(); self.stack.pop(); self.stack.push(0.0); self.stack.push(0.0); self.stack.push(0.0); self.stack.push(1.0); }
                Op::Transpose | Op::Inverse | Op::Determinant | Op::MatrixCompMult => {}
                Op::Vec2 | Op::Vec3 | Op::Vec4 => {
                    // Vector construction: N scalars on stack → 1 vector
                    // (we keep them on the stack as N consecutive slots)
                }
                Op::Swizzle => {}
                Op::Jump => ip = inst.arg as usize,
                Op::JumpIfFalse => {
                    if let Some(v) = self.stack.pop() {
                        if v == 0.0 {
                            ip = inst.arg as usize;
                        }
                    }
                }
                Op::JumpIfTrue => {
                    if let Some(v) = self.stack.pop() {
                        if v != 0.0 {
                            ip = inst.arg as usize;
                        }
                    }
                }
                Op::Call | Op::Return | Op::Discard => break,
                Op::Nop => {}
            }
        }
    }

    fn binary<F: FnOnce(f32, f32) -> f32>(&mut self, f: F) {
        let b = self.stack.pop().unwrap_or(0.0);
        let a = self.stack.pop().unwrap_or(0.0);
        self.stack.push(f(a, b));
    }

    fn unary<F: FnOnce(f32) -> f32>(&mut self, f: F) {
        let a = self.stack.pop().unwrap_or(0.0);
        self.stack.push(f(a));
    }

    fn pop(&mut self) -> f32 {
        self.stack.pop().unwrap_or(0.0)
    }
}
