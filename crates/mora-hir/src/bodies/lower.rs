//! Lowering of syntax to [`Body`], with the scopes of §7.10.

use mora_span::TextRange;
use mora_syntax::ast::{self, AstNode};
use mora_syntax::lexer::ident_name;
use mora_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::{
    Arg, ArgMode, BinaryOp, Body, BodySourceMap, Capture, CaptureTarget, Expr, ExprId, FileBodies,
    InterpPart, Lambda, Literal, Local, LocalFunction, LocalId, LocalKind, MatchArm, Pat, PatId,
    Path, ScopeData, ScopeId, Stmt, StmtId, TypeId, UnaryOp,
};
use crate::items::item_nodes;
use crate::type_ref::{LowerTypes, TypeRef, predefined};

/// Lowers every body of `file`.
pub(super) fn file(file: &ast::SourceFile) -> FileBodies {
    let items = item_nodes(file).iter().map(function).collect();
    let mut lower = Lower::new();
    let mut stmts = Vec::new();
    let mut range: Option<TextRange> = None;
    for top in file.top_level_items() {
        if let ast::TopLevelItem::Stmt(stmt) = top {
            let at = stmt.syntax().text_range();
            range = Some(range.map_or(at, |range| range.cover(at)));
            stmts.push(lower.stmt(&stmt));
        }
    }
    let root = lower.alloc_expr(Expr::Block(stmts), range.unwrap_or_default());
    lower.body.root = Some(root);
    FileBodies {
        top_level: lower.finish(),
        items,
    }
}

/// The body of a function item; `None` for any other item.
fn function(node: &SyntaxNode) -> Option<(Body, BodySourceMap)> {
    let function = ast::FunctionDecl::cast(node.clone())?;
    let mut lower = Lower::new();
    for param in function.param_list().iter().flat_map(|list| list.params()) {
        if let Some(local) = lower.declare(param.name(), LocalKind::Param, None) {
            lower.body.params.push(local);
        }
    }
    lower.body.root = lower.function_body(function.block(), function.expr_body());
    Some(lower.finish())
}

struct Lower {
    body: Body,
    map: BodySourceMap,
    types: LowerTypes,
    /// The innermost scope.
    scope: ScopeId,
}

impl Lower {
    fn new() -> Self {
        let mut body = Body::default();
        let scope = body.scopes.alloc(ScopeData {
            parent: None,
            local: None,
        });
        Self {
            body,
            map: BodySourceMap::default(),
            types: LowerTypes::default(),
            scope,
        }
    }

    fn finish(mut self) -> (Body, BodySourceMap) {
        self.map.type_paths = self.types.into_paths();
        (self.body, self.map)
    }

    fn alloc_expr(&mut self, expr: Expr, range: TextRange) -> ExprId {
        self.map.exprs.push(range);
        self.body.exprs.alloc(expr)
    }

    fn alloc_stmt(&mut self, stmt: Stmt) -> StmtId {
        self.body.stmts.alloc(stmt)
    }

    fn alloc_pat(&mut self, pat: Pat) -> PatId {
        self.body.pats.alloc(pat)
    }

    fn ty(&mut self, ty: Option<ast::Type>) -> TypeId {
        let ty = self.types.optional(ty.as_ref());
        self.body.types.alloc(ty)
    }

    fn opt_ty(&mut self, ty: Option<ast::Type>) -> Option<TypeId> {
        ty.map(|ty| self.ty(Some(ty)))
    }

    /// The local named `name` in scope, the innermost first.
    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.body
            .locals_in_scope(self.scope)
            .find(|&local| self.body.locals[local].name == name)
    }

    /// Declares a local in a new scope inside the current one. A local that
    /// takes the name of one in scope is recorded for `MR0180`. A name the
    /// parser recovered from declares nothing.
    fn declare(
        &mut self,
        name: Option<ast::Name>,
        kind: LocalKind,
        ty: Option<TypeId>,
    ) -> Option<LocalId> {
        let token = name?.ident_token()?;
        let name = ident_name(token.text()).into_owned();
        let earlier = self.lookup(&name);
        let local = self.body.locals.alloc(Local { name, kind, ty });
        self.map.locals.push(token.text_range());
        if let Some(earlier) = earlier {
            self.body.shadows.push((local, earlier));
        }
        self.scope = self.body.scopes.alloc(ScopeData {
            parent: Some(self.scope),
            local: Some(local),
        });
        Some(local)
    }

    /// Runs `f` and then leaves the scopes it opened.
    fn scoped<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.scope;
        let result = f(self);
        self.scope = saved;
        result
    }

    fn function_body(
        &mut self,
        block: Option<ast::Block>,
        expr: Option<ast::ExprBody>,
    ) -> Option<ExprId> {
        if let Some(block) = block {
            return Some(self.block_expr(&block));
        }
        let expr = expr?;
        Some(self.expr(expr.expr(), expr.syntax().text_range()))
    }

    fn block_expr(&mut self, block: &ast::Block) -> ExprId {
        let stmts = self.block_stmts(block);
        self.alloc_expr(Expr::Block(stmts), block.syntax().text_range())
    }

    fn block_stmts(&mut self, block: &ast::Block) -> Vec<StmtId> {
        self.scoped(|this| block.stmts().map(|stmt| this.stmt(&stmt)).collect())
    }

    fn opt_stmt(&mut self, stmt: Option<ast::Stmt>) -> StmtId {
        match stmt {
            Some(stmt) => self.stmt(&stmt),
            None => self.alloc_stmt(Stmt::Missing),
        }
    }

    /// A statement. The locals it declares stay in scope after it; the
    /// bindings of `is` patterns in it do not (D-132).
    fn stmt(&mut self, stmt: &ast::Stmt) -> StmtId {
        let lowered = match stmt {
            ast::Stmt::LocalDeclStmt(it) => return self.local_decl(it.local_decl()),
            ast::Stmt::DeconstructStmt(it) => {
                let init = self.scoped(|this| this.expr(it.expr(), it.syntax().text_range()));
                let kind = binding_kind(it.kw());
                let pat = match it.tuple_designation() {
                    Some(tuple) => self.tuple_designation(&tuple, kind),
                    None => self.alloc_pat(Pat::Missing),
                };
                Stmt::Deconstruct { pat, init }
            }
            ast::Stmt::LocalFunctionStmt(it) => match it.function_decl() {
                Some(function) => Stmt::Function(self.local_function(&function)),
                None => Stmt::Missing,
            },
            ast::Stmt::AttributedStmt(it) => return self.opt_stmt(it.stmt()),
            _ => self.scoped(|this| this.scoped_stmt(stmt)),
        };
        self.alloc_stmt(lowered)
    }

    /// A statement that declares nothing after itself.
    fn scoped_stmt(&mut self, stmt: &ast::Stmt) -> Stmt {
        let range = stmt.syntax().text_range();
        match stmt {
            ast::Stmt::Block(block) => Stmt::Block(self.block_stmts(block)),
            ast::Stmt::ExprStmt(it) => Stmt::Expr(self.expr(it.expr(), range)),
            ast::Stmt::EmptyStmt(_) => Stmt::Block(Vec::new()),
            ast::Stmt::IfStmt(it) => {
                let condition = self.expr(it.condition(), range);
                let then = self.opt_stmt(it.then());
                let r#else = it.else_clause().map(|clause| self.opt_stmt(clause.stmt()));
                Stmt::If {
                    condition,
                    then,
                    r#else,
                }
            }
            ast::Stmt::WhileStmt(it) => {
                let condition = self.expr(it.condition(), range);
                let body = self.opt_stmt(it.body());
                Stmt::While { condition, body }
            }
            ast::Stmt::DoWhileStmt(it) => {
                let body = self.opt_stmt(it.body());
                let condition = self.expr(it.condition(), range);
                Stmt::DoWhile { body, condition }
            }
            ast::Stmt::ForStmt(it) => {
                let init = match it.for_init() {
                    Some(ast::ForInit::LocalDecl(decl)) => vec![self.local_decl(Some(decl))],
                    Some(ast::ForInit::ForInitExprs(exprs)) => exprs
                        .exprs()
                        .map(|expr| {
                            let range = expr.syntax().text_range();
                            let expr = self.expr(Some(expr), range);
                            self.alloc_stmt(Stmt::Expr(expr))
                        })
                        .collect(),
                    None => Vec::new(),
                };
                let condition = it.condition().map(|expr| self.expr(Some(expr), range));
                let update = it
                    .for_update()
                    .iter()
                    .flat_map(|update| update.exprs())
                    .map(|expr| self.expr(Some(expr), range))
                    .collect();
                let body = self.opt_stmt(it.body());
                Stmt::For {
                    init,
                    condition,
                    update,
                    body,
                }
            }
            ast::Stmt::ForeachStmt(it) => {
                let iterable = self.expr(it.iterable(), range);
                let kind = binding_kind(it.kw());
                let pat = match it.designation() {
                    Some(designation) => self.designation(&designation, kind),
                    None => self.alloc_pat(Pat::Missing),
                };
                let body = self.opt_stmt(it.body());
                Stmt::Foreach {
                    pat,
                    by_ref: it.ref_token().is_some(),
                    iterable,
                    body,
                }
            }
            ast::Stmt::BreakStmt(_) => Stmt::Break,
            ast::Stmt::ContinueStmt(_) => Stmt::Continue,
            ast::Stmt::ReturnStmt(it) => {
                Stmt::Return(it.expr().map(|expr| self.expr(Some(expr), range)))
            }
            ast::Stmt::MatchStmt(it) => {
                let scrutinee = self.expr(it.scrutinee(), range);
                let arms = self.match_arms(it.match_arm_list());
                Stmt::Match { scrutinee, arms }
            }
            ast::Stmt::UnsafeStmt(it) => {
                let block = match it.block() {
                    Some(block) => {
                        let stmts = self.block_stmts(&block);
                        self.alloc_stmt(Stmt::Block(stmts))
                    }
                    None => self.alloc_stmt(Stmt::Missing),
                };
                Stmt::Unsafe(block)
            }
            // Lowered by `stmt`.
            ast::Stmt::LocalDeclStmt(_)
            | ast::Stmt::DeconstructStmt(_)
            | ast::Stmt::LocalFunctionStmt(_)
            | ast::Stmt::AttributedStmt(_) => Stmt::Missing,
        }
    }

    /// `let x: T = e`: the initializer is lowered before the local is
    /// declared, so it cannot see it.
    fn local_decl(&mut self, decl: Option<ast::LocalDecl>) -> StmtId {
        let Some(decl) = decl else {
            return self.alloc_stmt(Stmt::Missing);
        };
        let kind = match decl.kw().map(|kw| kw.kind()) {
            Some(SyntaxKind::VarKw) => LocalKind::Var,
            _ => LocalKind::Let,
        };
        let declarator = decl.variable_declarator();
        let ty = self.opt_ty(declarator.as_ref().and_then(|it| it.ty()));
        let init = declarator.as_ref().and_then(|it| it.init()).map(|init| {
            let range = init.syntax().text_range();
            self.scoped(|this| this.expr(Some(init), range))
        });
        let local = self.declare(declarator.and_then(|it| it.name()), kind, ty);
        match local {
            Some(local) => self.alloc_stmt(Stmt::Local { local, init }),
            None => match init {
                Some(init) => self.alloc_stmt(Stmt::Expr(init)),
                None => self.alloc_stmt(Stmt::Missing),
            },
        }
    }

    /// A local function: its name is in scope after it and in its own body.
    fn local_function(&mut self, function: &ast::FunctionDecl) -> LocalFunction {
        let ret = self.opt_ty(function.ret());
        let local = self.declare(function.name(), LocalKind::Function, None);
        let local = local.unwrap_or_else(|| {
            // A local function the parser recovered from still has a body
            // to lower; its name is the empty string, which no use matches.
            self.map.locals.push(function.syntax().text_range());
            self.body.locals.alloc(Local {
                name: String::new(),
                kind: LocalKind::Function,
                ty: None,
            })
        });
        let is_async = has_modifier(function.modifiers(), SyntaxKind::AsyncKw);
        self.scoped(|this| {
            let params = function
                .param_list()
                .iter()
                .flat_map(|list| list.params())
                .filter_map(|param| {
                    let ty = this.opt_ty(param.ty());
                    this.declare(param.name(), LocalKind::Param, ty)
                })
                .collect();
            let body = this.function_body(function.block(), function.expr_body());
            LocalFunction {
                local,
                params,
                ret,
                is_async,
                body,
            }
        })
    }

    fn match_arms(&mut self, list: Option<ast::MatchArmList>) -> Vec<MatchArm> {
        let Some(list) = list else {
            return Vec::new();
        };
        let mut arms = Vec::new();
        for child in list.syntax().children() {
            if let Some(arm) = ast::MatchArm::cast(child.clone()) {
                let range = arm.syntax().text_range();
                arms.push(self.scoped(|this| {
                    let pat = this.opt_pattern(arm.pattern());
                    let guard = this.guard(arm.when_clause());
                    let body = this.expr(arm.expr(), range);
                    MatchArm { pat, guard, body }
                }));
            } else if let Some(arm) = ast::MatchBlockArm::cast(child) {
                let range = arm.syntax().text_range();
                arms.push(self.scoped(|this| {
                    let pat = this.opt_pattern(arm.pattern());
                    let guard = this.guard(arm.when_clause());
                    let body = match arm.block() {
                        Some(block) => this.block_expr(&block),
                        None => this.alloc_expr(Expr::Missing, range),
                    };
                    MatchArm { pat, guard, body }
                }));
            }
        }
        arms
    }

    fn guard(&mut self, clause: Option<ast::WhenClause>) -> Option<ExprId> {
        let clause = clause?;
        let range = clause.syntax().text_range();
        Some(self.expr(clause.expr(), range))
    }

    /// An expression; `range` is where a missing one would have been.
    fn expr(&mut self, expr: Option<ast::Expr>, range: TextRange) -> ExprId {
        match expr {
            Some(expr) => self.lower_expr(&expr),
            None => self.alloc_expr(Expr::Missing, range),
        }
    }

    fn lower_expr(&mut self, expr: &ast::Expr) -> ExprId {
        let range = expr.syntax().text_range();
        let lowered = match expr {
            ast::Expr::LiteralExpr(it) => Expr::Literal(literal(it.value())),
            ast::Expr::InterpString(it) => {
                let parts = it
                    .interp_parts()
                    .map(|part| match part {
                        ast::InterpPart::InterpTextPart(text) => {
                            InterpPart::Text(text.syntax().text().to_string())
                        }
                        ast::InterpPart::InterpHole(hole) => {
                            let at = hole.syntax().text_range();
                            InterpPart::Hole {
                                value: self.expr(hole.value(), at),
                                alignment: hole.alignment().map(|e| self.expr(Some(e), at)),
                                format: hole
                                    .hole_format_token()
                                    .map(|token| token.text().to_owned()),
                            }
                        }
                    })
                    .collect();
                Expr::Interpolated(parts)
            }
            ast::Expr::PathExpr(it) => return self.path_expr(it.path(), range),
            ast::Expr::PredefinedTypeExpr(it) => {
                let ty = it
                    .predefined_type()
                    .and_then(|ty| ty.kind())
                    .map_or(TypeRef::Missing, |token| predefined(token.kind()));
                Expr::Type(self.body.types.alloc(ty))
            }
            ast::Expr::ThisExpr(_) => Expr::This,
            ast::Expr::ParenExpr(it) => return self.expr(it.expr(), range),
            ast::Expr::TupleExpr(it) => Expr::Tuple(
                it.tuple_elems()
                    .map(|elem| (name_ref(elem.name_ref()), self.expr(elem.expr(), range)))
                    .collect(),
            ),
            ast::Expr::CollectionExpr(it) => Expr::Collection(
                it.collection_elems()
                    .map(|elem| {
                        let spread = elem.dot_dot_token().is_some();
                        (spread, self.expr(elem.expr(), range))
                    })
                    .collect(),
            ),
            ast::Expr::MemberExpr(it) => {
                let receiver = self.expr(it.receiver(), range);
                let optional = it
                    .op()
                    .is_some_and(|op| op.kind() == SyntaxKind::QuestionDot);
                let args = self.generic_args(it.generic_arg_list());
                let Some(token) = it.name_ref().and_then(|name| name.ident_token()) else {
                    // `x.$name` inside a quote, or a name the parser
                    // recovered from.
                    return self.alloc_expr(Expr::Macro, range);
                };
                let member = Expr::Member {
                    receiver,
                    name: ident_name(token.text()).into_owned(),
                    optional,
                    args,
                };
                let id = self.alloc_expr(member, range);
                self.map.member_names.insert(id, token.text_range());
                return id;
            }
            ast::Expr::CallExpr(it) => {
                let callee = self.expr(it.callee(), range);
                let args = self.args(it.arg_list().iter().flat_map(|list| list.args()));
                Expr::Call { callee, args }
            }
            ast::Expr::IndexExpr(it) => {
                let base = self.expr(it.base(), range);
                let optional = it
                    .op()
                    .is_some_and(|op| op.kind() == SyntaxKind::QuestionLBracket);
                let args = self.args(it.args());
                Expr::Index {
                    base,
                    args,
                    optional,
                }
            }
            ast::Expr::PostfixExpr(it) => Expr::Postfix {
                increment: it.op().is_some_and(|op| op.kind() == SyntaxKind::PlusPlus),
                expr: self.expr(it.expr(), range),
            },
            ast::Expr::PropagateExpr(it) => Expr::Propagate(self.expr(it.expr(), range)),
            ast::Expr::PrefixExpr(it) => {
                let op = match it.op().map(|op| op.kind()) {
                    Some(SyntaxKind::Minus) => UnaryOp::Neg,
                    Some(SyntaxKind::Bang) => UnaryOp::Not,
                    Some(SyntaxKind::Tilde) => UnaryOp::BitNot,
                    Some(SyntaxKind::PlusPlus) => UnaryOp::PreIncrement,
                    Some(SyntaxKind::MinusMinus) => UnaryOp::PreDecrement,
                    Some(SyntaxKind::Caret) => UnaryOp::FromEnd,
                    _ => UnaryOp::Plus,
                };
                Expr::Unary {
                    op,
                    expr: self.expr(it.expr(), range),
                }
            }
            ast::Expr::CastExpr(it) => Expr::Cast {
                ty: self.ty(it.ty()),
                expr: self.expr(it.expr(), range),
            },
            ast::Expr::AwaitExpr(it) => Expr::Await(self.expr(it.expr(), range)),
            ast::Expr::RangeExpr(it) => Expr::Range {
                start: it.start().map(|e| self.expr(Some(e), range)),
                end: it.end().map(|e| self.expr(Some(e), range)),
            },
            ast::Expr::MatchExpr(it) => {
                let scrutinee = self.expr(it.scrutinee(), range);
                let arms = self.match_arms(it.match_arm_list());
                Expr::Match { scrutinee, arms }
            }
            ast::Expr::WithExpr(it) => {
                let expr = self.expr(it.expr(), range);
                let inits = self.inits(it.object_initializer()).unwrap_or_default();
                Expr::With { expr, inits }
            }
            ast::Expr::BinExpr(it) => {
                let lhs = self.expr(it.lhs(), range);
                let rhs = self.expr(it.rhs(), range);
                let op = it
                    .op()
                    .and_then(|op| binary_op(op.kind()))
                    .unwrap_or(BinaryOp::Add);
                Expr::Binary { op, lhs, rhs }
            }
            ast::Expr::IsExpr(it) => {
                let expr = self.expr(it.expr(), range);
                // The bindings stay in scope to the end of the statement.
                let pat = self.opt_pattern(it.pattern());
                Expr::Is { expr, pat }
            }
            ast::Expr::AsExpr(it) => Expr::As {
                expr: self.expr(it.expr(), range),
                ty: self.ty(it.ty()),
            },
            ast::Expr::ConditionalExpr(it) => Expr::Conditional {
                condition: self.expr(it.condition(), range),
                then: self.expr(it.then(), range),
                r#else: self.expr(it.r#else(), range),
            },
            ast::Expr::AssignExpr(it) => {
                let lhs = self.expr(it.lhs(), range);
                let rhs = self.expr(it.rhs(), range);
                let op = it.op().and_then(|op| assign_op(op.kind()));
                Expr::Assign { op, lhs, rhs }
            }
            ast::Expr::LambdaExpr(it) => {
                // The capture list names locals of the enclosing scope, so
                // it is lowered before the parameters.
                let captures = it
                    .capture_list()
                    .iter()
                    .flat_map(|list| list.captures())
                    .collect::<Vec<_>>();
                let captures = captures
                    .iter()
                    .map(|capture| self.capture(capture))
                    .collect::<Vec<_>>();
                let is_async = has_modifier(it.modifiers(), SyntaxKind::AsyncKw);
                let is_static = has_modifier(it.modifiers(), SyntaxKind::StaticKw);
                let (params, body) = self.scoped(|this| {
                    let params = it
                        .lambda_param()
                        .into_iter()
                        .chain(
                            it.lambda_param_list()
                                .into_iter()
                                .flat_map(|list| list.lambda_params()),
                        )
                        .filter_map(|param| {
                            let ty = this.opt_ty(param.ty());
                            this.declare(param.name(), LocalKind::Param, ty)
                        })
                        .collect();
                    let body = match it.block() {
                        Some(block) => this.block_expr(&block),
                        None => this.expr(it.expr(), range),
                    };
                    (params, body)
                });
                let lambda = Lambda {
                    params,
                    captures: captures
                        .iter()
                        .map(|(capture, _)| capture.clone())
                        .collect(),
                    is_async,
                    is_static,
                    body,
                };
                let id = self.alloc_expr(Expr::Lambda(lambda), range);
                for (index, (_, unresolved)) in captures.iter().enumerate() {
                    if let Some(at) = unresolved {
                        self.map.captures.insert((id, index), *at);
                    }
                }
                return id;
            }
            ast::Expr::NewExpr(it) => Expr::New {
                ty: self.opt_ty(it.ty()),
                args: it.arg_list().map(|list| self.args(list.args())),
                inits: self.inits(it.object_initializer()),
            },
            ast::Expr::NewArrayExpr(it) => Expr::NewArray {
                ty: self.ty(it.elem()),
                size: self.expr(it.size(), range),
                stack: false,
            },
            ast::Expr::StackallocExpr(it) => Expr::NewArray {
                ty: self.ty(it.elem()),
                size: self.expr(it.size(), range),
                stack: true,
            },
            ast::Expr::DefaultExpr(it) => Expr::Default(self.opt_ty(it.ty())),
            ast::Expr::NameofExpr(it) => Expr::Nameof(self.expr(it.expr(), range)),
            ast::Expr::SizeofExpr(it) => Expr::Sizeof(self.ty(it.ty())),
            ast::Expr::QuoteExpr(_) | ast::Expr::MacroCallExpr(_) | ast::Expr::SpliceExpr(_) => {
                Expr::Macro
            }
        };
        self.alloc_expr(lowered, range)
    }

    /// A path whose first segment is a local in scope is that local and
    /// member accesses on it; any other path is left for
    /// [`crate::body_resolution`].
    fn path_expr(&mut self, path: Option<ast::Path>, range: TextRange) -> ExprId {
        let mut segments = Vec::new();
        let mut ranges = Vec::new();
        let mut args = Vec::new();
        for segment in path.iter().flat_map(|path| path.path_segments()) {
            let Some(token) = segment.name_ref().and_then(|name| name.ident_token()) else {
                break;
            };
            segments.push(ident_name(token.text()).into_owned());
            ranges.push(token.text_range());
            args.push(self.generic_args(segment.generic_arg_list()));
        }
        let Some(first) = segments.first() else {
            return self.alloc_expr(Expr::Missing, range);
        };
        let Some(local) = self.lookup(first) else {
            let path = Path {
                segments,
                args,
                scope: self.scope,
            };
            let id = self.alloc_expr(Expr::Path(path), range);
            self.map.path_segments.insert(id, ranges);
            return id;
        };
        let start = range.start();
        let mut expr = self.alloc_expr(Expr::Local(local), ranges[0]);
        for ((name, args), at) in segments.into_iter().zip(args).zip(ranges).skip(1) {
            let member = Expr::Member {
                receiver: expr,
                name,
                optional: false,
                args,
            };
            expr = self.alloc_expr(member, TextRange::new(start, at.end()));
            self.map.member_names.insert(expr, at);
        }
        expr
    }

    fn generic_args(&mut self, list: Option<ast::GenericArgList>) -> Vec<TypeId> {
        list.iter()
            .flat_map(|list| list.types())
            .map(|ty| self.ty(Some(ty)))
            .collect()
    }

    fn args(&mut self, args: impl Iterator<Item = ast::Arg>) -> Vec<Arg> {
        args.map(|arg| {
            let mode = arg.mode().and_then(|mode| match mode.kind() {
                SyntaxKind::RefKw => Some(ArgMode::Ref),
                SyntaxKind::InKw => Some(ArgMode::In),
                SyntaxKind::MoveKw => Some(ArgMode::Move),
                _ => None,
            });
            let range = arg.syntax().text_range();
            Arg {
                name: name_ref(arg.name()),
                mode,
                expr: self.expr(arg.expr(), range),
            }
        })
        .collect()
    }

    fn inits(&mut self, list: Option<ast::ObjectInitializer>) -> Option<Vec<(String, ExprId)>> {
        let list = list?;
        Some(
            list.member_inits()
                .map(|init| {
                    let range = init.syntax().text_range();
                    let name = name_ref(init.name_ref()).unwrap_or_default();
                    (name, self.expr(init.expr(), range))
                })
                .collect(),
        )
    }

    /// A capture, and where its name is when it names no local in scope.
    fn capture(&mut self, capture: &ast::Capture) -> (Capture, Option<TextRange>) {
        let weak = capture
            .mode()
            .is_some_and(|mode| mode.kind() == SyntaxKind::WeakKw);
        let mut unresolved = None;
        let target = if capture.this_token().is_some() {
            CaptureTarget::This
        } else {
            let token = capture.name_ref().and_then(|name| name.ident_token());
            let name = token
                .as_ref()
                .map(|token| ident_name(token.text()).into_owned())
                .unwrap_or_default();
            match self.lookup(&name) {
                Some(local) => CaptureTarget::Local(local),
                None => {
                    unresolved = token.map(|token| token.text_range());
                    CaptureTarget::Unresolved(name)
                }
            }
        };
        (Capture { weak, target }, unresolved)
    }

    fn opt_pattern(&mut self, pattern: Option<ast::Pattern>) -> PatId {
        match pattern {
            Some(pattern) => self.pattern(&pattern),
            None => self.alloc_pat(Pat::Missing),
        }
    }

    fn pattern(&mut self, pattern: &ast::Pattern) -> PatId {
        let range = pattern.syntax().text_range();
        let lowered = match pattern {
            ast::Pattern::ConstantPattern(it) => Pat::Constant(self.expr(it.expr(), range)),
            ast::Pattern::TypePattern(it) => Pat::Type {
                ty: self.ty(it.ty()),
                binding: None,
            },
            ast::Pattern::DeclarationPattern(it) => {
                let ty = self.ty(it.ty());
                let binding = it
                    .designation()
                    .map(|designation| self.designation(&designation, LocalKind::Let));
                Pat::Type { ty, binding }
            }
            ast::Pattern::BindingPattern(it) => {
                let kind = binding_kind(it.kw());
                return match it.designation() {
                    Some(designation) => self.designation(&designation, kind),
                    None => self.alloc_pat(Pat::Missing),
                };
            }
            ast::Pattern::DiscardPattern(_) => Pat::Discard,
            ast::Pattern::RelationalPattern(it) => Pat::Relational {
                op: it
                    .op()
                    .and_then(|op| binary_op(op.kind()))
                    .unwrap_or(BinaryOp::Lt),
                value: self.expr(it.expr(), range),
            },
            ast::Pattern::NotPattern(it) => Pat::Not(self.opt_pattern(it.pattern())),
            ast::Pattern::BinPattern(it) => {
                let lhs = self.opt_pattern(it.lhs());
                let rhs = self.opt_pattern(it.rhs());
                if it.op().is_some_and(|op| op.kind() == SyntaxKind::OrKw) {
                    Pat::Or(lhs, rhs)
                } else {
                    Pat::And(lhs, rhs)
                }
            }
            ast::Pattern::ParenPattern(it) => return self.opt_pattern(it.pattern()),
            ast::Pattern::CasePattern(it) => {
                let path = self.path_expr(it.path(), range);
                let fields = self.fields(
                    it.positional_pattern()
                        .iter()
                        .flat_map(|p| p.pattern_fields()),
                );
                Pat::Case { path, fields }
            }
            ast::Pattern::PositionalPattern(it) => {
                Pat::Positional(self.fields(it.pattern_fields()))
            }
            ast::Pattern::PropertyPattern(it) => {
                let ty = self.opt_ty(it.ty());
                let fields = self.fields(it.pattern_fields());
                let binding = it
                    .designation()
                    .map(|designation| self.designation(&designation, LocalKind::Let));
                Pat::Member {
                    ty,
                    fields,
                    binding,
                }
            }
            ast::Pattern::ListPattern(it) => {
                Pat::List(it.patterns().map(|p| self.pattern(&p)).collect())
            }
            ast::Pattern::SlicePattern(it) => Pat::Slice(it.pattern().map(|p| self.pattern(&p))),
        };
        self.alloc_pat(lowered)
    }

    fn fields(
        &mut self,
        fields: impl Iterator<Item = ast::PatternField>,
    ) -> Vec<(Option<String>, PatId)> {
        fields
            .map(|field| {
                (
                    name_ref(field.name_ref()),
                    self.opt_pattern(field.pattern()),
                )
            })
            .collect()
    }

    fn designation(&mut self, designation: &ast::Designation, kind: LocalKind) -> PatId {
        match designation {
            ast::Designation::NameDesignation(it) => {
                let ty = self.opt_ty(it.ty());
                match self.declare(it.name(), kind, ty) {
                    Some(local) => self.alloc_pat(Pat::Bind(local)),
                    None => self.alloc_pat(Pat::Missing),
                }
            }
            ast::Designation::TupleDesignation(it) => self.tuple_designation(it, kind),
            ast::Designation::DiscardDesignation(_) => self.alloc_pat(Pat::Discard),
        }
    }

    fn tuple_designation(&mut self, tuple: &ast::TupleDesignation, kind: LocalKind) -> PatId {
        let elems = tuple
            .designations()
            .map(|designation| (None, self.designation(&designation, kind)))
            .collect();
        self.alloc_pat(Pat::Positional(elems))
    }
}

/// `var` binds a mutable local; `let`, or no keyword, an immutable one.
fn binding_kind(kw: Option<SyntaxToken>) -> LocalKind {
    match kw.map(|kw| kw.kind()) {
        Some(SyntaxKind::VarKw) => LocalKind::Var,
        _ => LocalKind::Let,
    }
}

fn has_modifier(modifiers: ast::AstChildren<ast::Modifier>, kind: SyntaxKind) -> bool {
    modifiers
        .filter_map(|modifier| modifier.kind())
        .any(|token| token.kind() == kind)
}

fn name_ref(name: Option<ast::NameRef>) -> Option<String> {
    let token = name?.ident_token()?;
    Some(ident_name(token.text()).into_owned())
}

fn literal(token: Option<SyntaxToken>) -> Literal {
    let Some(token) = token else {
        return Literal::None;
    };
    let text = token.text().to_owned();
    match token.kind() {
        SyntaxKind::IntNumber => Literal::Int(text),
        SyntaxKind::RealNumber => Literal::Real(text),
        SyntaxKind::Char => Literal::Char(text),
        SyntaxKind::TrueKw => Literal::Bool(true),
        SyntaxKind::FalseKw => Literal::Bool(false),
        SyntaxKind::NoneKw => Literal::None,
        _ => Literal::String(text),
    }
}

fn binary_op(kind: SyntaxKind) -> Option<BinaryOp> {
    use BinaryOp as B;
    use SyntaxKind as K;
    Some(match kind {
        K::Star => B::Mul,
        K::Slash => B::Div,
        K::Percent => B::Rem,
        K::StarPercent => B::WrappingMul,
        K::Plus => B::Add,
        K::Minus => B::Sub,
        K::PlusPercent => B::WrappingAdd,
        K::MinusPercent => B::WrappingSub,
        K::Shl => B::Shl,
        K::Shr => B::Shr,
        K::UShr => B::UShr,
        K::Lt => B::Lt,
        K::Gt => B::Gt,
        K::LtEq => B::Le,
        K::GtEq => B::Ge,
        K::EqEq => B::Eq,
        K::BangEq => B::Ne,
        K::Amp => B::BitAnd,
        K::Caret => B::BitXor,
        K::Pipe => B::BitOr,
        K::AmpAmp => B::And,
        K::PipePipe => B::Or,
        K::QuestionQuestion => B::Coalesce,
        _ => return None,
    })
}

/// The operator of a compound assignment; `None` for `=`.
fn assign_op(kind: SyntaxKind) -> Option<BinaryOp> {
    use BinaryOp as B;
    use SyntaxKind as K;
    Some(match kind {
        K::PlusEq => B::Add,
        K::MinusEq => B::Sub,
        K::StarEq => B::Mul,
        K::SlashEq => B::Div,
        K::PercentEq => B::Rem,
        K::AmpEq => B::BitAnd,
        K::PipeEq => B::BitOr,
        K::CaretEq => B::BitXor,
        K::ShlEq => B::Shl,
        K::ShrEq => B::Shr,
        K::UShrEq => B::UShr,
        K::QuestionQuestionEq => B::Coalesce,
        K::PlusPercentEq => B::WrappingAdd,
        K::MinusPercentEq => B::WrappingSub,
        K::StarPercentEq => B::WrappingMul,
        _ => return None,
    })
}
