//! GLSL AST → bytecode compiler.
//!
//! Takes our simplified [`ast::Shader`] and produces a
//! [`opcode::ShaderProgram`] that the interpreter can run.

use crate::glsl::ast::*;
use crate::glsl::opcode::*;
use std::collections::HashMap;

pub fn compile_shader(shader: &Shader) -> ShaderProgram {
    let mut prog = ShaderProgram::default();
    let symbols: HashMap<String, (DeclKind, u32)> = shader.symbols.clone();
    let builtins: HashMap<&str, u32> = [
        ("gl_Position", 0u32),
        ("gl_FragColor", 1),
        ("gl_FragCoord", 2),
        ("gl_PointCoord", 3),
        ("gl_PointSize", 4),
        ("gl_FragDepth", 5),
    ].iter().cloned().collect();
    let ctx = CompileCtx { symbols, builtins };
    for stmt in &shader.main_body {
        compile_statement(stmt, &mut prog, &ctx);
    }
    prog.entry = 0;
    prog
}

struct CompileCtx {
    symbols: HashMap<String, (DeclKind, u32)>,
    builtins: HashMap<&'static str, u32>,
}

fn compile_statement(stmt: &Statement, prog: &mut ShaderProgram, ctx: &CompileCtx) {
    match stmt {
        Statement::Expr(e) => {
            compile_expr(e, prog, ctx);
            emit(prog, Op::Pop, 0);
        }
        Statement::Return(e) => {
            if let Some(e) = e {
                compile_expr(e, prog, ctx);
            }
            emit(prog, Op::Return, 0);
        }
        Statement::Discard => { emit(prog, Op::Discard, 0); }
        Statement::Break => { emit(prog, Op::Jump, 0); }
        Statement::Continue => { emit(prog, Op::Jump, 0); }
        Statement::If { cond, then_body, else_body } => {
            compile_expr(cond, prog, ctx);
            let jump_if_false = emit(prog, Op::JumpIfFalse, 0);
            for s in then_body {
                compile_statement(s, prog, ctx);
            }
            let jump_end = if !else_body.is_empty() {
                let j = emit(prog, Op::Jump, 0);
                prog.instructions[jump_if_false as usize].arg = prog.instructions.len() as u32;
                for s in else_body {
                    compile_statement(s, prog, ctx);
                }
                j
            } else {
                prog.instructions[jump_if_false as usize].arg = prog.instructions.len() as u32;
                0
            };
            if jump_end != 0 {
                prog.instructions[jump_end as usize].arg = prog.instructions.len() as u32;
            }
        }
        Statement::Block(stmts) => {
            for s in stmts {
                compile_statement(s, prog, ctx);
            }
        }
        Statement::Declare { init, .. } => {
            if let Some(e) = init {
                compile_expr(e, prog, ctx);
                emit(prog, Op::Pop, 0);
            }
        }
        _ => {}
    }
}

fn compile_expr(e: &Expr, prog: &mut ShaderProgram, ctx: &CompileCtx) {
    match e {
        Expr::Float(f) => {
            let idx = prog.float_consts.len() as u32;
            prog.float_consts.push(*f);
            emit(prog, Op::PushFloat, idx);
            prog.instructions.last_mut().unwrap().float_val = *f;
        }
        Expr::Int(i) => {
            let idx = prog.int_consts.len() as u32;
            prog.int_consts.push(*i);
            emit(prog, Op::PushInt, idx);
        }
        Expr::Bool(b) => { emit(prog, Op::PushBool, if *b { 1 } else { 0 }); }
        Expr::Ident(name) => {
            // Resolve: builtin > symbol table > local.
            if let Some(&slot) = ctx.builtins.get(name.as_str()) {
                emit(prog, Op::LoadBuiltin, slot);
            } else if let Some((kind, loc)) = ctx.symbols.get(name) {
                match kind {
                    DeclKind::Attribute => { emit(prog, Op::LoadAttrib, *loc); }
                    DeclKind::Uniform => { emit(prog, Op::LoadUniform, *loc); }
                    DeclKind::Varying => { emit(prog, Op::LoadVarying, *loc); }
                    DeclKind::Const => { emit(prog, Op::Load, *loc); }
                }
            } else {
                // Unknown identifier — treat as local variable.
                let slot = hash_name(name);
                emit(prog, Op::Load, slot);
            }
        }
        Expr::Binary(op, a, b) => {
            compile_expr(a, prog, ctx);
            compile_expr(b, prog, ctx);
            let opcode = match op {
                BinaryOp::Add => Op::Add,
                BinaryOp::Sub => Op::Sub,
                BinaryOp::Mul => Op::Mul,
                BinaryOp::Div => Op::Div,
                BinaryOp::Mod => Op::Mod,
                BinaryOp::Equal => Op::Equal,
                BinaryOp::NotEqual => Op::NotEqual,
                BinaryOp::Less => Op::Less,
                BinaryOp::Greater => Op::Greater,
                BinaryOp::LessEq => Op::LessEq,
                BinaryOp::GreaterEq => Op::GreaterEq,
                BinaryOp::And => Op::And,
                BinaryOp::Or => Op::Or,
                _ => Op::Add,
            };
            emit(prog, opcode, 0);
        }
        Expr::Unary(op, inner) => {
            compile_expr(inner, prog, ctx);
            match op {
                UnaryOp::Neg => { emit(prog, Op::Neg, 0); }
                UnaryOp::Not => { emit(prog, Op::Not, 0); }
                UnaryOp::BitNot => { emit(prog, Op::Not, 0); }
            }
        }
        Expr::Call(name, args) => {
            for a in args {
                compile_expr(a, prog, ctx);
            }
            compile_builtin_call(name, args.len(), prog, ctx);
        }
        Expr::Assign(target, _op, val) => {
            compile_expr(val, prog, ctx);
            // Store into the target.
            if let Expr::Ident(name) = target.as_ref() {
                if let Some(&slot) = ctx.builtins.get(name.as_str()) {
                    emit(prog, Op::StoreBuiltin, slot);
                } else if let Some((kind, loc)) = ctx.symbols.get(name) {
                    match kind {
                        DeclKind::Varying => { emit(prog, Op::StoreVarying, *loc); }
                        _ => { emit(prog, Op::Store, *loc); }
                    }
                } else {
                    let slot = hash_name(name);
                    emit(prog, Op::Store, slot);
                }
            } else {
                emit(prog, Op::Pop, 0);
            }
        }
        Expr::Field(obj, field) => {
            // Swizzle: a.xyz, a.rgb, etc.
            compile_expr(obj, prog, ctx);
            let mask = swizzle_mask(field);
            emit(prog, Op::Swizzle, mask);
        }
        Expr::Index(_obj, _idx) => {
            // Array indexing — simplified, not fully supported.
            // Push 0 as a placeholder.
            emit(prog, Op::PushFloat, 0);
            prog.instructions.last_mut().unwrap().float_val = 0.0;
        }
        Expr::Ternary(cond, a, b) => {
            compile_expr(cond, prog, ctx);
            let jump_if_false = emit(prog, Op::JumpIfFalse, 0);
            compile_expr(a, prog, ctx);
            let jump_end = emit(prog, Op::Jump, 0);
            prog.instructions[jump_if_false as usize].arg = prog.instructions.len() as u32;
            compile_expr(b, prog, ctx);
            prog.instructions[jump_end as usize].arg = prog.instructions.len() as u32;
        }
    }
}

fn compile_builtin_call(name: &str, _nargs: usize, prog: &mut ShaderProgram, _ctx: &CompileCtx) {
    let op = match name {
        "dot" => Op::Dot,
        "cross" => Op::Cross,
        "normalize" => Op::Normalize,
        "length" => Op::Length,
        "mix" => Op::Mix,
        "clamp" => Op::Clamp,
        "smoothstep" => Op::SmoothStep,
        "step" => Op::Step,
        "fract" => Op::Fract,
        "floor" => Op::Floor,
        "ceil" => Op::Ceil,
        "abs" => Op::Abs,
        "sign" => Op::Sign,
        "sin" => Op::Sin,
        "cos" => Op::Cos,
        "tan" => Op::Tan,
        "asin" => Op::Asin,
        "acos" => Op::Acos,
        "atan" => Op::Atan,
        "pow" => Op::Pow,
        "exp" => Op::Exp,
        "log" => Op::Log,
        "exp2" => Op::Exp2,
        "log2" => Op::Log2,
        "sqrt" => Op::Sqrt,
        "min" => Op::Min,
        "max" => Op::Max,
        "distance" => Op::Distance,
        "reflect" => Op::Reflect,
        "refract" => Op::Refract,
        "faceforward" => Op::FaceForward,
        "texture2D" => Op::Texture2D,
        "textureCube" => Op::TextureCube,
        "transpose" => Op::Transpose,
        "inverse" => Op::Inverse,
        "determinant" => Op::Determinant,
        "matrixCompMult" => Op::MatrixCompMult,
        "vec2" => Op::Vec2,
        "vec3" => Op::Vec3,
        "vec4" => Op::Vec4,
        _ => Op::Nop,
    };
    emit(prog, op, 0);
}

/// Hash a name to a local variable slot (fallback when the name is not
/// in the symbol table).
fn hash_name(name: &str) -> u32 {
    let mut h: u32 = 0;
    for b in name.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h % 256
}

/// Encode a swizzle mask. Each char (x/y/z/w or r/g/b/a or s/t/p/q)
/// maps to a 2-bit index 0-3. Up to 4 chars → 8 bits, stored in arg.
fn swizzle_mask(field: &str) -> u32 {
    let mut mask: u32 = 0;
    let mut count = 0u32;
    for c in field.chars().take(4) {
        let idx = match c {
            'x' | 'r' | 's' => 0,
            'y' | 'g' | 't' => 1,
            'z' | 'b' | 'p' => 2,
            'w' | 'a' | 'q' => 3,
            _ => 0,
        };
        mask |= idx << (count * 2);
        count += 1;
    }
    mask | (count << 8)
}

fn emit(prog: &mut ShaderProgram, op: Op, arg: u32) -> u32 {
    let idx = prog.instructions.len() as u32;
    prog.instructions.push(Instruction { op, arg, float_val: 0.0 });
    idx
}
