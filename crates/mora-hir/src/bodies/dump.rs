//! A text form of the HIR of a file, for snapshot tests.
//!
//! Each body prints like the source, one statement per line. A local is
//! `name#n`, where `n` is its index in the body. A path that is not a local
//! is followed by what it names in braces, as `Console.WriteLine{external}`
//! or `X{field App.Point.X}`, and so is a member of a namespace or a type,
//! as `Color.Red{enum member App.Color.Red}`. A member without braces is a
//! member of a value, which type checking looks up. A local declared
//! without a type shows the type inference gives it, when that is known,
//! as `let total#0: int = 0;`, and an operation made only of literals that
//! took its target type is followed by its value, as `1 / 3{= 0}`. An
//! operand that is itself an operation is in parentheses, so the dump shows
//! how the expression was grouped.

use std::fmt::Write;

use mora_span::{Db, Project, SourceFile};

use super::{
    Arg, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, CaptureTarget, Expr, ExprId,
    InterpPart, Literal, LocalId, LocalKind, MatchArm, Pat, PatId, Stmt, StmtId, Target, TypeId,
    body, body_owners, body_resolution,
};
use crate::arena::arena_index;
use crate::inference::{Inference, infer, is_known, ty_name};
use crate::items::{ItemKind, ValueKind, item_tree};
use crate::report::{full_name, item_full_name};
use crate::resolve::{Resolved, Scope, item_data};
use crate::type_ref::TypeRef;

/// The HIR of every body of `file`: its top-level statements, then each
/// function in the order of the text.
pub fn hir_dump(db: &dyn Db, project: Project, file: SourceFile) -> String {
    let mut out = String::new();
    for owner in body_owners(db, file) {
        let body = body(db, file, owner);
        let mut printer = Printer {
            db,
            body,
            resolution: body_resolution(db, project, file, owner),
            inference: infer(db, project, file, owner),
            out: String::new(),
            indent: 0,
        };
        printer.owner(project, file, owner);
        // An owner without code, such as a field without an initializer,
        // prints nothing.
        if printer.out.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&printer.out);
    }
    out
}

struct Printer<'a> {
    db: &'a dyn Db,
    body: &'a Body,
    resolution: &'a BodyResolution,
    inference: &'a Inference,
    out: String,
    indent: usize,
}

impl Printer<'_> {
    fn owner(&mut self, project: Project, file: SourceFile, owner: BodyOwner) {
        let Some(root) = self.body.root else {
            return;
        };
        let scope = Scope::file(project, file);
        let item = |item| item_full_name(self.db, scope.item(item));
        let name = match owner {
            BodyOwner::TopLevel => {
                self.out.push_str("top-level ");
                self.statement_body(root);
                self.out.push('\n');
                return;
            }
            BodyOwner::Item(idx) => item(idx),
            BodyOwner::Value(ty, index) => {
                let values = &item_tree(self.db, file).item(ty).values;
                format!("{}.{}", item(ty), values[index as usize].name)
            }
            BodyOwner::Member(ty, _) => match self.body.kind {
                BodyKind::Constructor | BodyKind::Destructor => item(ty),
                _ => format!("{}.{}", item(ty), self.body.name),
            },
            BodyOwner::Extension(index, 0) => format!("extension#{index}"),
            BodyOwner::Extension(index, _) => format!("extension#{index}.{}", self.body.name),
        };
        let keyword = match self.body.kind {
            BodyKind::TopLevel | BodyKind::Function => "fn",
            BodyKind::Initializer => "init",
            BodyKind::Getter | BodyKind::Get => "get",
            BodyKind::Set => "set",
            BodyKind::Constructor => "new",
            BodyKind::Destructor => "drop",
            BodyKind::Operator => "operator",
            BodyKind::Extension => "extension",
            BodyKind::Case => "case",
        };
        let _ = write!(self.out, "{keyword} {name}");
        let callable = !matches!(self.body.kind, BodyKind::Initializer | BodyKind::Getter);
        if callable || !self.body.params.is_empty() {
            self.out.push('(');
            let body = self.body;
            self.params(&body.params, &body.defaults);
            self.out.push(')');
        }
        if let Some(call) = self.body.this_call {
            self.out.push_str(" : ");
            self.expr(call);
        }
        self.out.push(' ');
        self.statement_body(root);
        self.out.push('\n');
    }

    /// The body of a function: an expression body ends with `;`.
    fn statement_body(&mut self, expr: ExprId) {
        self.body_expr(expr);
        if !matches!(self.body.exprs[expr], Expr::Block(_)) {
            self.out.push(';');
        }
    }

    /// A function or lambda body: a block, or `=> e`.
    fn body_expr(&mut self, expr: ExprId) {
        if let Expr::Block(stmts) = &self.body.exprs[expr] {
            self.block(stmts);
        } else {
            self.out.push_str("=> ");
            self.expr(expr);
        }
    }

    fn line(&mut self) {
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
    }

    fn block(&mut self, stmts: &[StmtId]) {
        if stmts.is_empty() {
            self.out.push_str("{}");
            return;
        }
        self.out.push('{');
        self.indent += 1;
        for &stmt in stmts {
            self.line();
            self.stmt(stmt);
        }
        self.indent -= 1;
        self.line();
        self.out.push('}');
    }

    /// Parameters, each with its default value when it has one.
    fn params(&mut self, params: &[LocalId], defaults: &[(LocalId, ExprId)]) {
        for (index, &param) in params.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            self.local(param);
            if let Some(&(_, default)) = defaults.iter().find(|&&(local, _)| local == param) {
                self.out.push_str(" = ");
                self.expr(default);
            }
        }
    }

    fn local(&mut self, local: LocalId) {
        let name = &self.body.locals[local].name;
        let _ = write!(self.out, "{name}#{}", arena_index(local));
    }

    /// A local's declaration and its initializer, without the `;`.
    fn local_with_init(&mut self, local: LocalId, init: Option<ExprId>) {
        self.local_decl(local);
        if let Some(init) = init {
            self.out.push_str(" = ");
            self.expr(init);
        }
    }

    fn local_decl(&mut self, local: LocalId) {
        let data = &self.body.locals[local];
        let keyword = match data.kind {
            LocalKind::Var => "var ",
            _ => "let ",
        };
        self.out.push_str(keyword);
        self.local(local);
        if let Some(ty) = data.ty {
            self.out.push_str(": ");
            self.ty(ty);
        } else if let Some(ty) = self.inference.local(local).filter(|ty| is_known(ty)) {
            let _ = write!(self.out, ": {}", ty_name(self.db, ty));
        }
    }

    fn stmt(&mut self, stmt: StmtId) {
        match &self.body.stmts[stmt] {
            Stmt::Missing => self.out.push_str("<missing>;"),
            Stmt::Expr(expr) => {
                self.expr(*expr);
                self.out.push(';');
            }
            Stmt::Local { local, init } => {
                self.local_with_init(*local, *init);
                self.out.push(';');
            }
            Stmt::Deconstruct { pat, init } => {
                self.out.push_str("let ");
                self.pat(*pat);
                self.out.push_str(" = ");
                self.expr(*init);
                self.out.push(';');
            }
            Stmt::Function(function) => {
                self.local(function.local);
                self.out.push('(');
                self.params(&function.params, &function.defaults);
                self.out.push(')');
                if let Some(ret) = function.ret {
                    self.out.push_str(": ");
                    self.ty(ret);
                }
                self.out.push(' ');
                match function.body {
                    Some(body) => self.statement_body(body),
                    None => self.out.push(';'),
                }
            }
            Stmt::Block(stmts) => self.block(stmts),
            Stmt::If {
                condition,
                then,
                r#else,
            } => {
                self.out.push_str("if (");
                self.expr(*condition);
                self.out.push_str(") ");
                self.stmt(*then);
                if let Some(r#else) = r#else {
                    self.out.push_str(" else ");
                    self.stmt(*r#else);
                }
            }
            Stmt::While { condition, body } => {
                self.out.push_str("while (");
                self.expr(*condition);
                self.out.push_str(") ");
                self.stmt(*body);
            }
            Stmt::DoWhile { body, condition } => {
                self.out.push_str("do ");
                self.stmt(*body);
                self.out.push_str(" while (");
                self.expr(*condition);
                self.out.push_str(");");
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                self.out.push_str("for (");
                // The initializers are separated by commas, as written.
                for (index, &stmt) in init.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    match &self.body.stmts[stmt] {
                        Stmt::Expr(expr) => self.expr(*expr),
                        Stmt::Local { local, init } => self.local_with_init(*local, *init),
                        _ => self.out.push_str("<missing>"),
                    }
                }
                self.out.push_str("; ");
                if let Some(condition) = condition {
                    self.expr(*condition);
                }
                self.out.push(';');
                for (index, &expr) in update.iter().enumerate() {
                    self.out.push_str(if index > 0 { ", " } else { " " });
                    self.expr(expr);
                }
                self.out.push_str(") ");
                self.stmt(*body);
            }
            Stmt::Foreach {
                pat,
                by_ref,
                iterable,
                body,
            } => {
                self.out.push_str("foreach (");
                if *by_ref {
                    self.out.push_str("ref ");
                }
                self.pat(*pat);
                self.out.push_str(" in ");
                self.expr(*iterable);
                self.out.push_str(") ");
                self.stmt(*body);
            }
            Stmt::Break => self.out.push_str("break;"),
            Stmt::Continue => self.out.push_str("continue;"),
            Stmt::Return(expr) => {
                self.out.push_str("return");
                if let Some(expr) = expr {
                    self.out.push(' ');
                    self.expr(*expr);
                }
                self.out.push(';');
            }
            Stmt::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Stmt::Unsafe(block) => {
                self.out.push_str("unsafe ");
                self.stmt(*block);
            }
        }
    }

    fn r#match(&mut self, scrutinee: ExprId, arms: &[MatchArm]) {
        self.out.push_str("match (");
        self.expr(scrutinee);
        self.out.push_str(") {");
        self.indent += 1;
        for arm in arms {
            self.line();
            self.pat(arm.pat);
            if let Some(guard) = arm.guard {
                self.out.push_str(" when ");
                self.expr(guard);
            }
            self.out.push_str(" => ");
            self.expr(arm.body);
            self.out.push(',');
        }
        self.indent -= 1;
        self.line();
        self.out.push('}');
    }

    /// An operand: in parentheses when it is itself an operation.
    fn operand(&mut self, expr: ExprId) {
        let compound = matches!(
            self.body.exprs[expr],
            Expr::Binary { .. }
                | Expr::Assign { .. }
                | Expr::Conditional { .. }
                | Expr::Is { .. }
                | Expr::As { .. }
                | Expr::Range { .. }
                | Expr::Lambda(_)
                | Expr::Unary { .. }
                | Expr::Cast { .. }
                | Expr::Await(_)
                | Expr::With { .. }
        );
        if compound {
            self.out.push('(');
            self.expr(expr);
            self.out.push(')');
        } else {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: ExprId) {
        match &self.body.exprs[expr] {
            Expr::Missing => self.out.push_str("<missing>"),
            Expr::Literal(literal) => match literal {
                Literal::Int(text)
                | Literal::Real(text)
                | Literal::Char(text)
                | Literal::String(text) => self.out.push_str(text),
                Literal::Bool(value) => {
                    let _ = write!(self.out, "{value}");
                }
                Literal::None => self.out.push_str("none"),
            },
            Expr::Interpolated(parts) => {
                self.out.push_str("$\"");
                for part in parts {
                    match part {
                        InterpPart::Text(text) => self.out.push_str(text),
                        InterpPart::Hole {
                            value,
                            alignment,
                            format,
                        } => {
                            self.out.push('{');
                            self.expr(*value);
                            if let Some(alignment) = alignment {
                                self.out.push(',');
                                self.expr(*alignment);
                            }
                            if let Some(format) = format {
                                self.out.push_str(format);
                            }
                            self.out.push('}');
                        }
                    }
                }
                self.out.push('"');
            }
            Expr::Local(local) => self.local(*local),
            Expr::Path(path) => {
                let resolution = self.resolution.paths.get(&expr);
                let named = resolution.map_or(path.segments.len(), |r| r.segments.max(1));
                for (index, segment) in path.segments.iter().enumerate() {
                    if index > 0 {
                        self.out.push('.');
                    }
                    self.out.push_str(segment);
                    self.type_args(&path.args[index]);
                    if index + 1 == named {
                        let target =
                            resolution.map_or_else(|| "?".to_owned(), |r| self.target(&r.target));
                        let _ = write!(self.out, "{{{target}}}");
                    }
                }
            }
            Expr::This => self.out.push_str("this"),
            Expr::Type(ty) => self.ty(*ty),
            Expr::Tuple(elems) => {
                self.out.push('(');
                for (index, (name, expr)) in elems.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    if let Some(name) = name {
                        let _ = write!(self.out, "{name}: ");
                    }
                    self.expr(*expr);
                }
                if elems.len() == 1 {
                    self.out.push(',');
                }
                self.out.push(')');
            }
            Expr::Collection(elems) => {
                self.out.push('[');
                for (index, (spread, expr)) in elems.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    if *spread {
                        self.out.push_str("..");
                    }
                    self.expr(*expr);
                }
                self.out.push(']');
            }
            Expr::Member {
                receiver,
                name,
                optional,
                args,
            } => {
                self.operand(*receiver);
                self.out.push_str(if *optional { "?." } else { "." });
                self.out.push_str(name);
                self.type_args(args);
                if let Some(resolution) = self.resolution.paths.get(&expr) {
                    let target = self.target(&resolution.target);
                    let _ = write!(self.out, "{{{target}}}");
                }
            }
            Expr::Call { callee, args } => {
                self.operand(*callee);
                self.args('(', args, ')');
            }
            Expr::Index {
                base,
                args,
                optional,
            } => {
                self.operand(*base);
                self.args(if *optional { '?' } else { '[' }, args, ']');
            }
            Expr::Unary { op, expr } => {
                self.out.push_str(op.text());
                self.operand(*expr);
            }
            Expr::Postfix { increment, expr } => {
                self.operand(*expr);
                self.out.push_str(if *increment { "++" } else { "--" });
            }
            Expr::Binary { op, lhs, rhs } => {
                self.operand(*lhs);
                let _ = write!(self.out, " {} ", op.text());
                self.operand(*rhs);
            }
            Expr::Assign { op, lhs, rhs } => {
                self.operand(*lhs);
                let op = op.map_or("", BinaryOp::text);
                let _ = write!(self.out, " {op}= ");
                self.operand(*rhs);
            }
            Expr::Propagate(expr) => {
                self.operand(*expr);
                self.out.push('?');
            }
            Expr::Cast { ty, expr } => {
                self.out.push('(');
                self.ty(*ty);
                self.out.push(')');
                self.operand(*expr);
            }
            Expr::Await(expr) => {
                self.out.push_str("await ");
                self.operand(*expr);
            }
            Expr::Range { start, end } => {
                if let Some(start) = start {
                    self.operand(*start);
                }
                self.out.push_str("..");
                if let Some(end) = end {
                    self.operand(*end);
                }
            }
            Expr::Match { scrutinee, arms } => self.r#match(*scrutinee, arms),
            Expr::With { expr, inits } => {
                self.operand(*expr);
                self.out.push_str(" with ");
                self.inits(inits);
            }
            Expr::Is { expr, pat } => {
                self.operand(*expr);
                self.out.push_str(" is ");
                self.pat(*pat);
            }
            Expr::As { expr, ty } => {
                self.operand(*expr);
                self.out.push_str(" as ");
                self.ty(*ty);
            }
            Expr::Conditional {
                condition,
                then,
                r#else,
            } => {
                self.operand(*condition);
                self.out.push_str(" ? ");
                self.operand(*then);
                self.out.push_str(" : ");
                self.operand(*r#else);
            }
            Expr::Lambda(lambda) => {
                if !lambda.captures.is_empty() {
                    self.out.push('[');
                    for (index, capture) in lambda.captures.iter().enumerate() {
                        if index > 0 {
                            self.out.push_str(", ");
                        }
                        self.out
                            .push_str(if capture.weak { "weak " } else { "move " });
                        match &capture.target {
                            CaptureTarget::This => self.out.push_str("this"),
                            CaptureTarget::Local(local) => self.local(*local),
                            CaptureTarget::Unresolved(name) => {
                                let _ = write!(self.out, "{name}{{error}}");
                            }
                        }
                    }
                    self.out.push_str("] ");
                }
                if lambda.is_static {
                    self.out.push_str("static ");
                }
                if lambda.is_async {
                    self.out.push_str("async ");
                }
                self.out.push('(');
                self.params(&lambda.params, &lambda.defaults);
                self.out.push_str(") ");
                self.body_expr(lambda.body);
            }
            Expr::New { ty, args, inits } => {
                self.out.push_str("new");
                if let Some(ty) = ty {
                    self.out.push(' ');
                    self.ty(*ty);
                }
                if let Some(args) = args {
                    self.args('(', args, ')');
                }
                if let Some(inits) = inits {
                    self.out.push(' ');
                    self.inits(inits);
                }
            }
            Expr::NewArray { ty, size, stack } => {
                self.out
                    .push_str(if *stack { "stackalloc " } else { "new " });
                self.ty(*ty);
                self.out.push('[');
                self.expr(*size);
                self.out.push(']');
            }
            Expr::Default(ty) => {
                self.out.push_str("default");
                if let Some(ty) = ty {
                    self.out.push('(');
                    self.ty(*ty);
                    self.out.push(')');
                }
            }
            Expr::Nameof(expr) => {
                self.out.push_str("nameof(");
                self.expr(*expr);
                self.out.push(')');
            }
            Expr::Sizeof(ty) => {
                self.out.push_str("sizeof(");
                self.ty(*ty);
                self.out.push(')');
            }
            Expr::Block(stmts) => self.block(stmts),
            Expr::Macro => self.out.push_str("<macro>"),
        }
        if let Some(value) = self.inference.constant(expr) {
            let _ = write!(self.out, "{{= {value}}}");
        }
    }

    fn args(&mut self, open: char, args: &[Arg], close: char) {
        self.out.push(open);
        if open == '?' {
            self.out.push('[');
        }
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            if let Some(name) = &arg.name {
                let _ = write!(self.out, "{name}: ");
            }
            if let Some(mode) = arg.mode {
                let _ = write!(self.out, "{} ", mode.keyword());
            }
            self.expr(arg.expr);
        }
        self.out.push(close);
    }

    fn inits(&mut self, inits: &[(String, ExprId)]) {
        if inits.is_empty() {
            self.out.push_str("{}");
            return;
        }
        self.out.push_str("{ ");
        for (index, (name, expr)) in inits.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            let _ = write!(self.out, "{name} = ");
            self.expr(*expr);
        }
        self.out.push_str(" }");
    }

    fn type_args(&mut self, args: &[TypeId]) {
        if args.is_empty() {
            return;
        }
        self.out.push('<');
        for (index, &ty) in args.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            self.ty(ty);
        }
        self.out.push('>');
    }

    fn fields(&mut self, open: char, fields: &[(Option<String>, PatId)], close: char) {
        self.out.push(open);
        for (index, (name, pat)) in fields.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            if let Some(name) = name {
                let _ = write!(self.out, "{name}: ");
            }
            self.pat(*pat);
        }
        self.out.push(close);
    }

    fn pat(&mut self, pat: PatId) {
        match &self.body.pats[pat] {
            Pat::Missing => self.out.push_str("<missing>"),
            Pat::Constant(expr) => self.expr(*expr),
            Pat::Type { ty, binding } => {
                self.ty(*ty);
                // `User u`: the binding is written without a keyword.
                if let Some(binding) = *binding {
                    self.out.push(' ');
                    match self.body.pats[binding] {
                        Pat::Bind(local) => self.local(local),
                        _ => self.pat(binding),
                    }
                }
            }
            Pat::Bind(local) => self.local_decl(*local),
            Pat::Discard => self.out.push('_'),
            Pat::Relational { op, value } => {
                let _ = write!(self.out, "{} ", op.text());
                self.expr(*value);
            }
            Pat::Not(inner) => {
                self.out.push_str("not ");
                self.pat(*inner);
            }
            Pat::And(lhs, rhs) | Pat::Or(lhs, rhs) => {
                let op = if matches!(self.body.pats[pat], Pat::And(..)) {
                    "and"
                } else {
                    "or"
                };
                self.out.push('(');
                self.pat(*lhs);
                let _ = write!(self.out, " {op} ");
                self.pat(*rhs);
                self.out.push(')');
            }
            Pat::Case { path, fields } => {
                self.expr(*path);
                self.fields('(', fields, ')');
            }
            Pat::Positional(fields) => self.fields('(', fields, ')'),
            Pat::Member {
                ty,
                fields,
                binding,
            } => {
                if let Some(ty) = ty {
                    self.ty(*ty);
                    self.out.push(' ');
                }
                self.fields('{', fields, '}');
                if let Some(binding) = binding {
                    self.out.push(' ');
                    self.pat(*binding);
                }
            }
            Pat::List(items) => {
                self.out.push('[');
                for (index, &item) in items.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    self.pat(item);
                }
                self.out.push(']');
            }
            Pat::Slice(inner) => {
                self.out.push_str("..");
                if let Some(inner) = inner {
                    self.out.push(' ');
                    self.pat(*inner);
                }
            }
        }
    }

    fn ty(&mut self, ty: TypeId) {
        let text = type_text(&self.body.types[ty]);
        self.out.push_str(&text);
    }

    /// What a path names, as `kind Full.Name`.
    fn target(&self, target: &Target) -> String {
        match target {
            Target::Error => "error".to_owned(),
            Target::Item(Resolved::External) => "external".to_owned(),
            Target::Item(resolved) => {
                let kind = match resolved {
                    Resolved::Namespace(_) => "namespace",
                    Resolved::Functions(items) if items.len() > 1 => "overloads",
                    Resolved::Functions(_) => "fn",
                    Resolved::Item(item) => match item_data(self.db, *item).kind {
                        ItemKind::Struct => "struct",
                        ItemKind::Class => "class",
                        ItemKind::Union => "union",
                        ItemKind::Enum => "enum",
                        ItemKind::Interface => "interface",
                        ItemKind::Function => "fn",
                        ItemKind::Macro => "macro",
                        ItemKind::Field => "value",
                    },
                    Resolved::External => "external",
                };
                let name = full_name(self.db, resolved).unwrap_or_default();
                format!("{kind} {name}")
            }
            Target::Value(value) => {
                let Some(member) = value.member(self.db) else {
                    return "error".to_owned();
                };
                let kind = match member.kind {
                    ValueKind::Field => "field",
                    ValueKind::Getter => "getter",
                    ValueKind::Const => "const",
                    ValueKind::EnumMember => "enum member",
                    ValueKind::UnionCase => "case",
                };
                let ty = item_full_name(self.db, value.ty);
                format!("{kind} {ty}.{}", member.name)
            }
        }
    }
}

/// A type as written.
fn type_text(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Primitive(primitive) => primitive.keyword().to_owned(),
        TypeRef::String => "string".to_owned(),
        TypeRef::Path(path) => {
            let mut text = path.segments.join(".");
            if !path.args.is_empty() {
                let args: Vec<_> = path.args.iter().map(type_text).collect();
                let _ = write!(text, "<{}>", args.join(", "));
            }
            text
        }
        TypeRef::Tuple(elems) => {
            let elems: Vec<_> = elems
                .iter()
                .map(|(name, ty)| match name {
                    Some(name) => format!("{name}: {}", type_text(ty)),
                    None => type_text(ty),
                })
                .collect();
            format!("({})", elems.join(", "))
        }
        TypeRef::Array(elem) => format!("{}[]", type_text(elem)),
        TypeRef::Optional(elem) => format!("{}?", type_text(elem)),
        TypeRef::Function(params, ret) => {
            let params: Vec<_> = params.iter().map(type_text).collect();
            format!("({}) => {}", params.join(", "), type_text(ret))
        }
        TypeRef::Weak(elem) => format!("weak {}", type_text(elem)),
        TypeRef::Any(elem) => format!("any {}", type_text(elem)),
        TypeRef::Splice => "$type".to_owned(),
        TypeRef::Missing => "<missing>".to_owned(),
    }
}
