//! Simplified GLSL AST.
//!
//! We translate the `glsl` crate's detailed AST into this simpler form
//! before compiling to bytecode. This keeps the compiler small and
//! focused on what the interpreter actually needs.

#[derive(Debug, Clone)]
pub struct Shader {
    pub stage: ShaderStage,
    pub declarations: Vec<Declaration>,
    pub main_body: Vec<Statement>,
    /// Symbol table: name → (kind, location slot). Used by the compiler
    /// to resolve identifiers to attribute/uniform/varying slots.
    pub symbols: std::collections::HashMap<String, (DeclKind, u32)>,
    pub uniform_count: u32,
    pub attrib_count: u32,
    pub varying_count: u32,
}

impl Shader {
    pub fn new(stage: ShaderStage) -> Self {
        Self {
            stage,
            declarations: Vec::new(),
            main_body: Vec::new(),
            symbols: std::collections::HashMap::new(),
            uniform_count: 0,
            attrib_count: 0,
            varying_count: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclKind {
    Uniform,
    Attribute,
    Varying,
    Const,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
}

#[derive(Debug, Clone)]
pub enum Declaration {
    Uniform { name: String, ty: Type, location: u32 },
    Attribute { name: String, ty: Type, location: u32 },
    Varying { name: String, ty: Type, location: u32 },
    Function { name: String, params: Vec<Param>, return_ty: Type, body: Vec<Statement> },
    Global { name: String, ty: Type, init: Option<Expr> },
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub qualifier: ParamQualifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamQualifier {
    In,
    Out,
    Inout,
}

#[derive(Debug, Clone)]
pub enum Statement {
    Expr(Expr),
    Declare { name: String, ty: Type, init: Option<Expr> },
    Assign { target: Expr, op: AssignOp, value: Expr },
    If { cond: Expr, then_body: Vec<Statement>, else_body: Vec<Statement> },
    For { init: Box<Statement>, cond: Expr, update: Box<Statement>, body: Vec<Statement> },
    While { cond: Expr, body: Vec<Statement> },
    DoWhile { body: Vec<Statement>, cond: Expr },
    Return(Option<Expr>),
    Break,
    Continue,
    Discard,
    Block(Vec<Statement>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Float(f32),
    Int(i32),
    Bool(bool),
    Ident(String),
    Field(Box<Expr>, String),           // a.b
    Index(Box<Expr>, Box<Expr>),        // a[i]
    Call(String, Vec<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>), // cond ? a : b
    Assign(Box<Expr>, AssignOp, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Equal, NotEqual, Less, Greater, LessEq, GreaterEq,
    And, Or, BitAnd, BitOr, BitXor, ShiftLeft, ShiftRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Void,
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    IVec2,
    IVec3,
    IVec4,
    BVec2,
    BVec3,
    BVec4,
    Mat2,
    Mat3,
    Mat4,
    Sampler2D,
    SamplerCube,
}

impl Type {
    /// Number of float components (for stack accounting).
    pub fn components(&self) -> usize {
        match self {
            Type::Float | Type::Int | Type::Bool => 1,
            Type::Vec2 | Type::IVec2 | Type::BVec2 => 2,
            Type::Vec3 | Type::IVec3 | Type::BVec3 => 3,
            Type::Vec4 | Type::IVec4 | Type::BVec4 => 4,
            Type::Mat2 => 4,
            Type::Mat3 => 9,
            Type::Mat4 => 16,
            _ => 0,
        }
    }
}
