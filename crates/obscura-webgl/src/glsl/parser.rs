//! GLSL parser bridge: uses the `glsl` crate to parse source, then
//! translates the `glsl` crate's AST into our simplified [`ast::Shader`].

use glsl::parser::Parse;
use glsl::syntax;

use crate::glsl::ast::*;

/// Parse GLSL source into our simplified AST.
pub fn parse_glsl(source: &str, stage: ShaderStage) -> Result<Shader, String> {
    let tu: syntax::TranslationUnit = syntax::TranslationUnit::parse(source)
        .map_err(|e| format!("GLSL parse error: {e}"))?;

    let mut shader = Shader::new(stage);

    for decl in tu.0 .0.iter() {
        translate_external_declaration(decl, &mut shader);
    }

    Ok(shader)
}

fn translate_external_declaration(decl: &syntax::ExternalDeclaration, shader: &mut Shader) {
    use syntax::ExternalDeclaration as Ext;
    match decl {
        Ext::FunctionDefinition(fd) => {
            if fd.prototype.name.as_str() == "main" {
                shader.main_body = translate_compound(&fd.statement);
            } else {
                let return_ty = translate_type(&fd.prototype.ty.ty.ty);
                shader.declarations.push(Declaration::Function {
                    name: fd.prototype.name.as_str().to_string(),
                    params: Vec::new(),
                    return_ty,
                    body: translate_compound(&fd.statement),
                });
            }
        }
        Ext::Declaration(d) => translate_declaration(d, shader),
        Ext::Preprocessor(_) => {}
    }
}

fn translate_declaration(d: &syntax::Declaration, shader: &mut Shader) {
    use syntax::Declaration as D;
    match d {
        D::InitDeclaratorList(init) => {
            // `uniform vec4 u_color;` or `attribute vec3 aPosition;` etc.
            let single = &init.head;
            let ty = translate_type(&single.ty.ty.ty);
            let name = single.name.as_ref().map(|n| n.as_str().to_string());
            if let Some(name) = name {
                let qualifier = single.ty.qualifier.as_ref().and_then(|q| {
                    q.qualifiers.0.first().and_then(|spec| {
                        match spec {
                            syntax::TypeQualifierSpec::Storage(s) => match s {
                                syntax::StorageQualifier::Uniform => Some(DeclKind::Uniform),
                                syntax::StorageQualifier::Attribute => Some(DeclKind::Attribute),
                                syntax::StorageQualifier::Varying => Some(DeclKind::Varying),
                                syntax::StorageQualifier::Const => Some(DeclKind::Const),
                                _ => None,
                            },
                            _ => None,
                        }
                    })
                });
                if let Some(kind) = qualifier {
                    let location = match kind {
                        DeclKind::Uniform => shader.uniform_count,
                        DeclKind::Attribute => shader.attrib_count,
                        DeclKind::Varying => shader.varying_count,
                        DeclKind::Const => 0,
                    };
                    let decl = match kind {
                        DeclKind::Uniform => {
                            shader.uniform_count += ty.components() as u32 + 1;
                            Declaration::Uniform { name: name.clone(), ty, location }
                        }
                        DeclKind::Attribute => {
                            shader.attrib_count += ty.components() as u32 + 1;
                            Declaration::Attribute { name: name.clone(), ty, location }
                        }
                        DeclKind::Varying => {
                            shader.varying_count += ty.components() as u32 + 1;
                            Declaration::Varying { name: name.clone(), ty, location }
                        }
                        DeclKind::Const => {
                            Declaration::Global { name: name.clone(), ty, init: None }
                        }
                    };
                    shader.declarations.push(decl);
                    // Also record name → (kind, location) in the symbol table.
                    shader.symbols.insert(name, (kind, location));
                } else {
                    shader.declarations.push(Declaration::Global { name, ty, init: None });
                }
            }
        }
        D::Global(_, _) => {
            // Global qualifier declaration without init — skip for now.
        }
        D::FunctionPrototype(_) => {}
        D::Precision(_, _) => {}
        D::Block(_) => {}
    }
}

fn translate_compound(cs: &syntax::CompoundStatement) -> Vec<Statement> {
    cs.statement_list.iter().filter_map(translate_statement).collect()
}

fn translate_statement(stmt: &syntax::Statement) -> Option<Statement> {
    use syntax::Statement as S;
    match stmt {
        S::Compound(c) => Some(Statement::Block(translate_compound(c))),
        S::Simple(s) => translate_simple(s),
    }
}

fn translate_simple(s: &syntax::SimpleStatement) -> Option<Statement> {
    use syntax::SimpleStatement as SS;
    match s {
        SS::Declaration(_) => None, // TODO: local declarations
        SS::Expression(e) => e.as_ref().map(|e| Statement::Expr(translate_expr(e))),
        SS::Selection(sel) => translate_selection(sel),
        SS::Iteration(_) => None, // TODO: for/while/do-while
        SS::Jump(j) => translate_jump(j),
        SS::Switch(_) => None,
        SS::CaseLabel(_) => None,
    }
}

fn translate_selection(sel: &syntax::SelectionStatement) -> Option<Statement> {
    let cond = translate_expr(&sel.cond);
    let (then_body, else_body) = match &sel.rest {
        syntax::SelectionRestStatement::Statement(s) => {
            let body = translate_statement(s).map(|st| vec![st]).unwrap_or_default();
            (body, Vec::new())
        }
        syntax::SelectionRestStatement::Else(then_stmt, else_stmt) => {
            let then_body = translate_statement(then_stmt).map(|st| vec![st]).unwrap_or_default();
            let else_body = translate_statement(else_stmt).map(|st| vec![st]).unwrap_or_default();
            (then_body, else_body)
        }
    };
    Some(Statement::If { cond, then_body, else_body })
}

fn translate_jump(j: &syntax::JumpStatement) -> Option<Statement> {
    use syntax::JumpStatement as J;
    match j {
        J::Return(e) => Some(Statement::Return(e.as_ref().map(|e| translate_expr(e)))),
        J::Break => Some(Statement::Break),
        J::Continue => Some(Statement::Continue),
        J::Discard => Some(Statement::Discard),
    }
}

fn translate_expr(e: &syntax::Expr) -> Expr {
    use syntax::Expr as E;
    match e {
        E::FloatConst(f) => Expr::Float(*f),
        E::DoubleConst(f) => Expr::Float(*f as f32),
        E::IntConst(i) => Expr::Int(*i),
        E::UIntConst(u) => Expr::Int(*u as i32),
        E::BoolConst(b) => Expr::Bool(*b),
        E::Variable(n) => Expr::Ident(n.as_str().to_string()),
        E::Unary(op, inner) => {
            match op {
                syntax::UnaryOp::Inc => {
                    // ++i → i = i + 1 (pre-increment)
                    let inner_expr = translate_expr(inner);
                    return Expr::Assign(
                        Box::new(inner_expr),
                        AssignOp::AddAssign,
                        Box::new(Expr::Int(1)),
                    );
                }
                syntax::UnaryOp::Dec => {
                    let inner_expr = translate_expr(inner);
                    return Expr::Assign(
                        Box::new(inner_expr),
                        AssignOp::SubAssign,
                        Box::new(Expr::Int(1)),
                    );
                }
                syntax::UnaryOp::Add => return translate_expr(inner),
                syntax::UnaryOp::Minus => Expr::Unary(UnaryOp::Neg, Box::new(translate_expr(inner))),
                syntax::UnaryOp::Not => Expr::Unary(UnaryOp::Not, Box::new(translate_expr(inner))),
                syntax::UnaryOp::Complement => Expr::Unary(UnaryOp::BitNot, Box::new(translate_expr(inner))),
            }
        }
        E::Binary(op, a, b) => {
            let bop = match op {
                syntax::BinaryOp::Add => BinaryOp::Add,
                syntax::BinaryOp::Sub => BinaryOp::Sub,
                syntax::BinaryOp::Mult => BinaryOp::Mul,
                syntax::BinaryOp::Div => BinaryOp::Div,
                syntax::BinaryOp::Mod => BinaryOp::Mod,
                syntax::BinaryOp::Equal => BinaryOp::Equal,
                syntax::BinaryOp::NonEqual => BinaryOp::NotEqual,
                syntax::BinaryOp::LT => BinaryOp::Less,
                syntax::BinaryOp::GT => BinaryOp::Greater,
                syntax::BinaryOp::LTE => BinaryOp::LessEq,
                syntax::BinaryOp::GTE => BinaryOp::GreaterEq,
                syntax::BinaryOp::And => BinaryOp::And,
                syntax::BinaryOp::Or => BinaryOp::Or,
                syntax::BinaryOp::BitAnd => BinaryOp::BitAnd,
                syntax::BinaryOp::BitOr => BinaryOp::BitOr,
                syntax::BinaryOp::BitXor => BinaryOp::BitXor,
                syntax::BinaryOp::Xor => BinaryOp::BitXor,
                syntax::BinaryOp::LShift => BinaryOp::ShiftLeft,
                syntax::BinaryOp::RShift => BinaryOp::ShiftRight,
            };
            Expr::Binary(bop, Box::new(translate_expr(a)), Box::new(translate_expr(b)))
        }
        E::Assignment(target, op, val) => {
            let aop = match op {
                syntax::AssignmentOp::Equal => AssignOp::Assign,
                syntax::AssignmentOp::Add => AssignOp::AddAssign,
                syntax::AssignmentOp::Sub => AssignOp::SubAssign,
                syntax::AssignmentOp::Mult => AssignOp::MulAssign,
                syntax::AssignmentOp::Div => AssignOp::DivAssign,
                syntax::AssignmentOp::Mod => AssignOp::Assign,
                _ => AssignOp::Assign,
            };
            Expr::Assign(Box::new(translate_expr(target)), aop, Box::new(translate_expr(val)))
        }
        E::Ternary(c, a, b) => Expr::Ternary(
            Box::new(translate_expr(c)),
            Box::new(translate_expr(a)),
            Box::new(translate_expr(b)),
        ),
        E::FunCall(ident, args) => {
            let name = match ident {
                syntax::FunIdentifier::Identifier(n) => n.as_str().to_string(),
                syntax::FunIdentifier::Expr(e) => match translate_expr(e) {
                    Expr::Ident(s) => s,
                    _ => "unknown".to_string(),
                },
            };
            let args: Vec<Expr> = args.iter().map(translate_expr).collect();
            Expr::Call(name, args)
        }
        E::Dot(obj, name) => Expr::Field(Box::new(translate_expr(obj)), name.as_str().to_string()),
        E::Bracket(obj, _arr) => Expr::Index(Box::new(translate_expr(obj)), Box::new(Expr::Int(0))),
        E::PostInc(obj) => {
            // i++ → i = i + 1
            let inner = translate_expr(obj);
            Expr::Assign(
                Box::new(inner.clone()),
                AssignOp::AddAssign,
                Box::new(Expr::Int(1)),
            )
        }
        E::PostDec(obj) => {
            let inner = translate_expr(obj);
            Expr::Assign(
                Box::new(inner.clone()),
                AssignOp::SubAssign,
                Box::new(Expr::Int(1)),
            )
        }
        E::Comma(a, _b) => translate_expr(a),
        E::BoolConst(b) => Expr::Bool(*b),
        E::IntConst(i) => Expr::Int(*i),
        E::UIntConst(u) => Expr::Int(*u as i32),
        E::FloatConst(f) => Expr::Float(*f),
        E::DoubleConst(f) => Expr::Float(*f as f32),
    }
}

fn translate_type(ty: &syntax::TypeSpecifierNonArray) -> Type {
    use syntax::TypeSpecifierNonArray as T;
    match ty {
        T::Void => Type::Void,
        T::Float => Type::Float,
        T::Int => Type::Int,
        T::Bool => Type::Bool,
        T::Vec2 => Type::Vec2,
        T::Vec3 => Type::Vec3,
        T::Vec4 => Type::Vec4,
        T::IVec2 => Type::IVec2,
        T::IVec3 => Type::IVec3,
        T::IVec4 => Type::IVec4,
        T::BVec2 => Type::BVec2,
        T::BVec3 => Type::BVec3,
        T::BVec4 => Type::BVec4,
        T::Mat2 => Type::Mat2,
        T::Mat3 => Type::Mat3,
        T::Mat4 => Type::Mat4,
        T::Sampler2D => Type::Sampler2D,
        T::SamplerCube => Type::SamplerCube,
        _ => Type::Float,
    }
}
