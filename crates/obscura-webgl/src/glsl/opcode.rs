//! Bytecode opcodes for the GLSL interpreter.
//!
//! The compiler turns GLSL AST into a flat list of these opcodes.
//! The interpreter is a stack machine: most ops pop operands, compute,
//! and push the result.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    // Stack
    PushFloat,    // push a float constant
    PushInt,      // push an int constant
    PushBool,     // push a bool constant
    Pop,          // discard top of stack

    // Variables (index into a flat "register" array)
    Load,         // push variable[idx]
    Store,        // pop and store into variable[idx]
    LoadAttrib,   // load vertex attribute[idx] (vertex shader only)
    LoadUniform,  // load uniform[idx]
    LoadVarying,  // load varying[idx] (fragment shader only)
    LoadBuiltin,  // load builtin[idx] (gl_FragCoord, gl_PointCoord, etc.)
    StoreVarying, // pop and store into varying[idx] (vertex shader)
    StoreBuiltin, // pop and store into builtin[idx] (gl_Position, gl_FragColor, etc.)

    // Arithmetic (binary, float)
    Add, Sub, Mul, Div, Mod,
    // Arithmetic (vector * scalar, matrix * vector, etc.) — same op, type inferred at runtime
    // Comparison
    Equal, NotEqual, Less, Greater, LessEq, GreaterEq,
    // Logical
    And, Or, Not,
    // Math functions
    Neg, Abs, Sign, Floor, Ceil, Fract,
    Sin, Cos, Tan, Asin, Acos, Atan,
    Pow, Exp, Log, Exp2, Log2, Sqrt,
    Mix, Clamp, SmoothStep, Step,
    Length, Distance, Normalize, Dot, Cross, Reflect, Refract, FaceForward,
    Min, Max,
    // Vector ops
    Vec2, Vec3, Vec4,         // construct vector from top N scalars
    Swizzle,                  // apply swizzle mask (encoded in arg)
    MatrixCompMult, Transpose, Inverse, Determinant,
    // Texture
    Texture2D, TextureCube,
    // Control flow
    Jump,                     // unconditional jump to addr
    JumpIfFalse,              // pop bool; jump if false
    JumpIfTrue,               // pop bool; jump if true
    Call,                     // call function at addr
    Return,                   // return from function
    Discard,                  // fragment shader: discard current fragment
    // No-op (padding for jump targets)
    Nop,
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub op: Op,
    /// Argument: constant index, variable index, jump target, swizzle mask, etc.
    pub arg: u32,
    /// For PushFloat, the float value is stored here (arg indexes a const pool).
    pub float_val: f32,
}

#[derive(Debug, Default)]
pub struct ShaderProgram {
    pub instructions: Vec<Instruction>,
    pub float_consts: Vec<f32>,
    pub int_consts: Vec<i32>,
    /// Entry point offset (main function).
    pub entry: u32,
}
