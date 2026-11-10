//! The text form of MIR bodies, for snapshot tests and `--emit mir`.
//!
//! It sits next to the HIR dump (`mora_hir::hir_dump`) and follows its
//! conventions: a resolved item is `Name{kind path}`, as in
//! `App.Fib{fn App.Fib}`, a standard-library function is
//! `Console.WriteLine{external}`, and a type is written as code writes it.
//! A local with a source name is `name#n`, where `n` is the index of the
//! HIR local it comes from, so it reads the same in both dumps; the
//! receiver, which no HIR local stands for, is `this`. Any other local is
//! `_n`, where `n` is its index in the body, so `_0` is the return slot and
//! the parameters come next. The `let` lines list the locals in the body's
//! order, which shows the mapping. The output is a pure function of the
//! body and the database: no map is iterated, so the same input gives the
//! same bytes.
//!
//! # Layout
//!
//! A body is a header, one line per local, and its blocks:
//!
//! ```text
//! fn Fib(n#0: int) -> int {
//!     let _0: int;
//!     let n#0: int;
//!     let mut acc#1: int;
//!     let _3: bool;
//!
//!     bb0: {
//!         _3 = copy n#0 < 2; // 3:9
//!         switch copy _3 [true -> bb1] otherwise bb2; // 3:5
//!     }
//!
//!     bb1: {
//!         _0 = copy n#0; // 4:16
//!         return; // 4:9
//!     }
//! }
//! ```
//!
//! The header is `fn name(params) -> ret {`, each parameter as `local: type`
//! and `ret` the type of `_0`. A local line is `let [mut] local: type;`. A
//! type is written as the HIR dump writes it, with `in ` or `ref ` before
//! the type of a borrow. A block is `bbN: {`, its statements, its
//! terminator, and `}`, one per line, with a blank line before each block.
//! Lowering numbers a body's blocks without gaps. A block with no statements
//! that ends in `unreachable` is left out of the dump, and the other blocks
//! keep their numbers. A real constant is written with a decimal point, as
//! `5.0`, in the shortest digits of its type. Every statement and terminator ends in
//! `// line:col`, the one-based line and the one-based column, counted in
//! characters as diagnostics count it, where its source starts, or `// ?`
//! when the span names no position in the file (see [`write_body_in`]).
//!
//! # Statements
//!
//! * `place = rvalue;`, `storage_live(local);`, `storage_dead(local);` and
//!   `nop;`.
//!
//! # Places
//!
//! A place is its local followed by its projections. A field of a struct is
//! `x#0.X`, by name, and a field of a tuple is `t#1.0`, by index. A
//! dereference is `(*this)`, so a field behind a borrow is
//! `(*this).X`.
//!
//! # Operands and constants
//!
//! An operand is `copy place`, `move place` or a constant as written: `3`,
//! `1.5`, `true`, `'a'`, `"text"` with `\"`, `\\`, `\n`, `\r`, `\t`, `\0`
//! and `\u{n}` escapes, and `()`. A constant does not show its type.
//!
//! # Values
//!
//! * `copy p`, `move p` and a constant: the operand itself.
//! * `&in p` and `&ref p`: a borrow of a place.
//! * `a + b`: a binary operation, written with the operator of the
//!   language: `+ - * / %`, `+% -% *%` for the wrapping forms,
//!   `<< >> >>>`, `& | ^` and `== != < <= > >=`.
//! * `-a`, `!a` and `~a`: a unary operation.
//! * `(type)a` and `(implicit type)a`: a numeric conversion written out,
//!   and one that keeps every value.
//! * `App.P{struct App.P}(a, b)`: a struct built from its parts, and
//!   `(a, b)`, or `(a,)` for one part, a tuple.
//! * `$"text{a}text"`: an interpolated string, with `{` and `}` in the
//!   text doubled.
//!
//! # Terminators
//!
//! * `goto bbN;` and `return;` and `unreachable;`.
//! * `switch operand [const -> bbN, ...] otherwise bbM;`.
//! * `[place = ]call callee(args) -> bbN;`, with `-> !` when the call does
//!   not return. The callee is an item as above, the name of the body for
//!   a local function (`Owner.Local`, from [`write_mir`]; `local#n` when
//!   the other bodies are not known). Local functions of one name in the
//!   same body, in blocks side by side, are `Owner.Local`, then
//!   `Owner.Local#2` and so on in the order of the text. The callee is
//!   `Item{kind path}.X{init}` for the
//!   initializer of a field (by index when the item has no such field), or
//!   the path of a standard-library function with `{external}`.

use std::fmt::Write;

use mora_diag::line_column;
use mora_hir::{
    Constant, Signature, Ty, arena_index, escape_char, item_text, real_text, signature, ty_name,
};
use mora_span::{Db, FileId, LineIndex, SourceFile, line_index};

use crate::{
    AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const, InterpPiece,
    LocalId, LocalSource, LocalTy, Mir, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, TerminatorKind, UnOp,
};

/// Appends the text of `body` to `out`. No file is known, so every span
/// prints as `// ?`, and a call of a local function prints its index;
/// [`write_body_in`] prints the positions and [`write_mir`] the names.
pub fn write_body(out: &mut String, db: &dyn Db, body: &Body) {
    Printer {
        db,
        body,
        bodies: &[],
        lines: None,
        out,
    }
    .body();
}

/// Appends the text of `body` to `out`, with each span as the line and
/// column in `file`. A span of another file, or past the end of `file`,
/// prints as `// ?`.
pub fn write_body_in(out: &mut String, db: &dyn Db, file: SourceFile, body: &Body) {
    Printer {
        db,
        body,
        bodies: &[],
        lines: Some((file.id(db), file.text(db), line_index(db, file))),
        out,
    }
    .body();
}

/// Appends the text of every body of `mir` to `out`, one blank line between
/// them. A call of a local function prints the name of the body it targets.
/// Spans print as in [`write_body_in`] when `file` is given, else as `// ?`.
pub fn write_mir(out: &mut String, db: &dyn Db, file: Option<SourceFile>, mir: &Mir) {
    for (index, body) in mir.bodies.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        Printer {
            db,
            body,
            bodies: &mir.bodies,
            lines: file.map(|file| (file.id(db), file.text(db).as_str(), line_index(db, file))),
            out,
        }
        .body();
    }
}

struct Printer<'a> {
    db: &'a dyn Db,
    body: &'a Body,
    /// The bodies a [`Callee::Local`] indexes, when known.
    bodies: &'a [Body],
    lines: Option<(FileId, &'a str, &'a LineIndex)>,
    out: &'a mut String,
}

impl Printer<'_> {
    fn body(&mut self) {
        let body = self.body;
        let _ = write!(self.out, "fn {}(", body.name);
        for index in 1..=body.params {
            if index > 1 {
                self.out.push_str(", ");
            }
            let id = LocalId(index as u32);
            let _ = write!(
                self.out,
                "{}: {}",
                self.local_name(id),
                self.local_ty(&body.local(id).ty)
            );
        }
        let ret = self.local_ty(&body.local(LocalId::RETURN).ty);
        let _ = writeln!(self.out, ") -> {ret} {{");
        for (index, decl) in body.locals.iter().enumerate() {
            let id = LocalId(index as u32);
            let _ = writeln!(
                self.out,
                "    let {}{}: {};",
                if decl.mutable { "mut " } else { "" },
                self.local_name(id),
                self.local_ty(&decl.ty)
            );
        }
        for id in body.block_ids() {
            let block = body.block(id);
            if is_empty_unreachable(block) {
                continue;
            }
            let _ = write!(self.out, "\n    bb{}: {{\n", id.0);
            for statement in &block.statements {
                self.statement(statement);
            }
            self.terminator(&block.terminator);
            self.out.push_str("    }\n");
        }
        self.out.push_str("}\n");
    }

    fn local_name(&self, id: LocalId) -> String {
        match &self.body.local(id).source {
            Some(LocalSource {
                name,
                hir: Some(hir),
            }) => format!("{name}#{}", arena_index(*hir)),
            Some(LocalSource { name, hir: None }) => name.clone(),
            None => format!("_{}", id.0),
        }
    }

    fn local_ty(&self, ty: &LocalTy) -> String {
        match ty {
            LocalTy::Value(ty) => ty_name(self.db, ty),
            LocalTy::Borrow(mode, ty) => format!("{} {}", borrow_word(*mode), ty_name(self.db, ty)),
        }
    }

    /// `// line:col`, or `// ?`.
    fn span(&mut self, span: mora_span::Span) {
        let position = self.lines.and_then(|(file, text, index)| {
            if span.file != file {
                return None;
            }
            line_column(text, index, usize::from(span.range.start()))
        });
        match position {
            Some((line, column)) => {
                let _ = writeln!(self.out, " // {line}:{column}");
            }
            None => self.out.push_str(" // ?\n"),
        }
    }

    fn statement(&mut self, statement: &Statement) {
        self.out.push_str("        ");
        match &statement.kind {
            StatementKind::Assign(place, rvalue) => {
                let place = self.place(place);
                let rvalue = self.rvalue(rvalue);
                let _ = write!(self.out, "{place} = {rvalue};");
            }
            StatementKind::StorageLive(local) => {
                let _ = write!(self.out, "storage_live({});", self.local_name(*local));
            }
            StatementKind::StorageDead(local) => {
                let _ = write!(self.out, "storage_dead({});", self.local_name(*local));
            }
            StatementKind::Nop => self.out.push_str("nop;"),
        }
        self.span(statement.span);
    }

    fn terminator(&mut self, terminator: &Terminator) {
        self.out.push_str("        ");
        match &terminator.kind {
            TerminatorKind::Goto(target) => {
                let _ = write!(self.out, "goto bb{};", target.0);
            }
            TerminatorKind::Switch {
                discr,
                targets,
                otherwise,
            } => {
                let targets: Vec<String> = targets
                    .iter()
                    .map(|(value, BlockId(target))| format!("{} -> bb{target}", constant(value)))
                    .collect();
                let _ = write!(
                    self.out,
                    "switch {} [{}] otherwise bb{};",
                    self.operand(discr),
                    targets.join(", "),
                    otherwise.0
                );
            }
            TerminatorKind::Return => self.out.push_str("return;"),
            TerminatorKind::Call {
                callee,
                args,
                dest,
                next,
            } => {
                if let Some(dest) = dest {
                    let _ = write!(self.out, "{} = ", self.place(dest));
                }
                let args: Vec<String> = args.iter().map(|arg| self.operand(arg)).collect();
                let target = match next {
                    Some(next) => format!("bb{}", next.0),
                    None => "!".to_owned(),
                };
                let _ = write!(
                    self.out,
                    "call {}({}) -> {target};",
                    self.callee(callee),
                    args.join(", ")
                );
            }
            TerminatorKind::Unreachable => self.out.push_str("unreachable;"),
        }
        self.span(terminator.span);
    }

    fn callee(&self, callee: &Callee) -> String {
        match callee {
            Callee::Item(item) => item_text(self.db, *item),
            Callee::Local(index) => match self.bodies.get(*index) {
                Some(body) => body.name.clone(),
                None => format!("local#{index}"),
            },
            Callee::Initializer(item, index) => {
                let name = match signature(self.db, *item) {
                    Signature::Struct(fields) => {
                        fields.get(*index as usize).map(|field| field.name.clone())
                    }
                    _ => None,
                };
                format!(
                    "{}.{}{{init}}",
                    item_text(self.db, *item),
                    name.unwrap_or_else(|| index.to_string())
                )
            }
            Callee::External(path) => format!("{}{{external}}", path.join(".")),
        }
    }

    fn rvalue(&self, rvalue: &Rvalue) -> String {
        match rvalue {
            Rvalue::Use(operand) => self.operand(operand),
            Rvalue::Borrow(mode, place) => format!("&{} {}", borrow_word(*mode), self.place(place)),
            Rvalue::Binary(op, left, right) => format!(
                "{} {} {}",
                self.operand(left),
                binary_symbol(*op),
                self.operand(right)
            ),
            Rvalue::Unary(op, operand) => {
                format!("{}{}", unary_symbol(*op), self.operand(operand))
            }
            Rvalue::Cast(kind, operand, ty) => {
                let prefix = match kind {
                    CastKind::Implicit => "implicit ",
                    CastKind::Numeric => "",
                };
                format!(
                    "({prefix}{}){}",
                    ty_name(self.db, ty),
                    self.operand(operand)
                )
            }
            Rvalue::Aggregate(kind, parts) => {
                let parts: Vec<String> = parts.iter().map(|part| self.operand(part)).collect();
                match kind {
                    AggregateKind::Struct(item) => {
                        format!("{}({})", item_text(self.db, *item), parts.join(", "))
                    }
                    AggregateKind::Tuple if parts.len() == 1 => format!("({},)", parts[0]),
                    AggregateKind::Tuple => format!("({})", parts.join(", ")),
                }
            }
            Rvalue::Interpolate(pieces) => {
                let mut text = String::from("$\"");
                for piece in pieces {
                    match piece {
                        InterpPiece::Text(literal) => {
                            for ch in literal.chars() {
                                match ch {
                                    '{' => text.push_str("{{"),
                                    '}' => text.push_str("}}"),
                                    _ => escape_char(ch, '"', &mut text),
                                }
                            }
                        }
                        InterpPiece::Operand {
                            value,
                            alignment,
                            format,
                        } => {
                            let _ = write!(text, "{{{}", self.operand(value));
                            if let Some(alignment) = alignment {
                                let _ = write!(text, ",{}", self.operand(alignment));
                            }
                            if let Some(format) = format {
                                let _ = write!(text, ":{format}");
                            }
                            text.push('}');
                        }
                    }
                }
                text.push('"');
                text
            }
        }
    }

    fn operand(&self, operand: &Operand) -> String {
        match operand {
            Operand::Copy(place) => format!("copy {}", self.place(place)),
            Operand::Move(place) => format!("move {}", self.place(place)),
            Operand::Const(value) => constant(value),
        }
    }

    /// The place, its fields by name where the type is a struct. The type
    /// is followed step by step and gives up, printing indexes, where the
    /// body is malformed.
    fn place(&self, place: &Place) -> String {
        let mut text = self.local_name(place.local);
        let mut ty = match &self.body.local(place.local).ty {
            LocalTy::Value(ty) | LocalTy::Borrow(_, ty) => Some(ty.clone()),
        };
        for projection in &place.projections {
            match *projection {
                Projection::Deref => text = format!("(*{text})"),
                Projection::Field(index) => {
                    let (name, field_ty) = self.field(ty.as_ref(), index);
                    let _ = write!(text, ".{name}");
                    ty = field_ty;
                }
            }
        }
        text
    }

    /// The name and the type of the `index`-th field of `ty`; the index and
    /// no type when `ty` is not a struct or tuple with such a field.
    fn field(&self, ty: Option<&Ty>, index: u32) -> (String, Option<Ty>) {
        match ty {
            Some(Ty::Struct(item)) => {
                if let Signature::Struct(fields) = signature(self.db, *item)
                    && let Some(field) = fields.get(index as usize)
                {
                    return (field.name.clone(), Some(field.ty.clone()));
                }
            }
            Some(Ty::Tuple(elems)) => {
                if let Some(elem) = elems.get(index as usize) {
                    return (index.to_string(), Some(elem.ty.clone()));
                }
            }
            _ => {}
        }
        (index.to_string(), None)
    }
}

/// A block with no statement that ends in `unreachable`; the dump leaves it
/// out.
fn is_empty_unreachable(block: &Block) -> bool {
    block.statements.is_empty() && matches!(block.terminator.kind, TerminatorKind::Unreachable)
}

fn borrow_word(mode: BorrowMode) -> &'static str {
    match mode {
        BorrowMode::In => "in",
        BorrowMode::Ref => "ref",
    }
}

fn binary_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::WrappingAdd => "+%",
        BinOp::WrappingSub => "-%",
        BinOp::WrappingMul => "*%",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::UShr => ">>>",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}

fn unary_symbol(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
    }
}

/// A constant as written.
fn constant(value: &Const) -> String {
    match value {
        Const::Int(value, _) => Constant::Int(*value).to_string(),
        Const::Real(bits, ty) => real_text(*bits, *ty),
        Const::Bool(value) => value.to_string(),
        Const::Char(ch) => {
            let mut text = String::from("'");
            escape_char(*ch, '\'', &mut text);
            text.push('\'');
            text
        }
        Const::String(value) => {
            let mut text = String::from("\"");
            for ch in value.chars() {
                escape_char(ch, '"', &mut text);
            }
            text.push('"');
            text
        }
        Const::Unit => "()".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, app, file_named};
    use mora_hir::{BodyOwner, ItemId, Primitive, TupleElem, Ty, body, item_tree};
    use mora_span::{FileId, Span, TextRange, TextSize};

    use super::*;
    use crate::{
        AggregateKind, BinOp, Block, BlockId, BorrowMode, Callee, CastKind, Const, InterpPiece,
        LocalDecl, LocalId, LocalSource, LocalTy, Mir, Operand, Place, Projection, Rvalue,
        Statement, StatementKind, Terminator, TerminatorKind, UnOp,
    };

    const SOURCE: &str = "struct P {\n    public X: int;\n    public Y: bool;\n}\n\nFib(n: int): int {\n    let a = n;\n    let b = a;\n    let c = b;\n    return c;\n}\n";

    struct Fx {
        db: TestDb,
        file: SourceFile,
        p: ItemId,
        fib: ItemId,
        hir: Vec<mora_hir::LocalId>,
    }

    fn fixture() -> Fx {
        let db = TestDb::new();
        let project = app(&db, &[("src/Program.mr", SOURCE)]);
        let file = file_named(&db, project, "src/Program.mr");
        let tree = item_tree(&db, file);
        let find = |name: &str| {
            let (item, _) = tree
                .items()
                .find(|(_, item)| item.name == name)
                .expect("the item");
            ItemId {
                project,
                file,
                item,
            }
        };
        let (p, fib) = (find("P"), find("Fib"));
        let hir: Vec<_> = body(&db, file, BodyOwner::Item(fib.item))
            .locals
            .iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(hir.len(), 4);
        Fx {
            db,
            file,
            p,
            fib,
            hir,
        }
    }

    /// The span of the first `needle` in the source.
    fn at(fx: &Fx, needle: &str) -> Span {
        let start = SOURCE.find(needle).expect("the needle") as u32;
        Span::new(
            fx.file.id(&fx.db),
            TextRange::new(TextSize::new(start), TextSize::new(start + 1)),
        )
    }

    fn nowhere() -> Span {
        Span::new(FileId::new(900), TextRange::empty(TextSize::new(0)))
    }

    fn int() -> Ty {
        Ty::Primitive(Primitive::Int)
    }

    fn named(fx: &Fx, name: &str, hir: usize, ty: LocalTy, mutable: bool) -> LocalDecl {
        LocalDecl {
            ty,
            mutable,
            source: Some(LocalSource {
                name: name.into(),
                hir: Some(fx.hir[hir]),
            }),
        }
    }

    fn temp(ty: Ty) -> LocalDecl {
        LocalDecl {
            ty: LocalTy::Value(ty),
            mutable: false,
            source: None,
        }
    }

    fn local(n: u32) -> Place {
        Place::local(LocalId(n))
    }

    fn copy(n: u32) -> Operand {
        Operand::Copy(local(n))
    }

    fn int_const(value: i128) -> Operand {
        Operand::Const(Const::Int(value, Primitive::Int))
    }

    fn assign(place: Place, rvalue: Rvalue, span: Span) -> Statement {
        Statement {
            kind: StatementKind::Assign(place, rvalue),
            span,
        }
    }

    fn end(kind: TerminatorKind, span: Span) -> Terminator {
        Terminator { kind, span }
    }

    fn block(statements: Vec<Statement>, terminator: Terminator) -> Block {
        Block {
            statements,
            terminator,
        }
    }

    fn body_of(name: &str, params: usize, locals: Vec<LocalDecl>, blocks: Vec<Block>) -> Body {
        Body {
            name: name.into(),
            params,
            locals,
            blocks,
            span: nowhere(),
        }
    }

    /// A body with one block holding `statements`, then `return`.
    fn one_block(fx: &Fx, statements: Vec<Statement>) -> Body {
        let mut locals = vec![temp(Ty::UNIT)];
        locals.extend((1..8).map(|_| temp(int())));
        let _ = fx;
        body_of(
            "F",
            0,
            locals,
            vec![block(statements, end(TerminatorKind::Return, nowhere()))],
        )
    }

    fn dump_in(fx: &Fx, body: &Body) -> String {
        let mut out = String::new();
        write_body_in(&mut out, &fx.db, fx.file, body);
        out
    }

    fn dump(fx: &Fx, body: &Body) -> String {
        let mut out = String::new();
        write_body(&mut out, &fx.db, body);
        out
    }

    /// The text of the statement `_1 = rvalue;` of a body, spans unknown.
    fn rvalue_text(fx: &Fx, rvalue: Rvalue) -> String {
        let body = one_block(fx, vec![assign(local(1), rvalue, nowhere())]);
        let text = dump(fx, &body);
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with("_1 = "))
            .expect("the statement");
        line.trim_start()
            .strip_prefix("_1 = ")
            .and_then(|rest| rest.strip_suffix("; // ?"))
            .expect("the shape")
            .to_owned()
    }

    fn hand_built(fx: &Fx) -> Body {
        let p = Ty::Struct(fx.p);
        let n = at(fx, "n: int");
        let x = at(fx, "X: int");
        let fib = at(fx, "Fib");
        let callee = Callee::Item(fx.fib);
        body_of(
            "Fib",
            2,
            vec![
                temp(int()),
                named(fx, "n", 0, LocalTy::Value(int()), false),
                LocalDecl {
                    ty: LocalTy::Borrow(BorrowMode::In, p),
                    mutable: false,
                    source: Some(LocalSource {
                        name: "this".into(),
                        hir: None,
                    }),
                },
                named(fx, "acc", 1, LocalTy::Value(int()), true),
                temp(Ty::Primitive(Primitive::Bool)),
            ],
            vec![
                block(
                    vec![
                        Statement {
                            kind: StatementKind::StorageLive(LocalId(4)),
                            span: n,
                        },
                        assign(
                            local(4),
                            Rvalue::Binary(BinOp::Lt, copy(1), int_const(2)),
                            n,
                        ),
                    ],
                    end(
                        TerminatorKind::Switch {
                            discr: copy(4),
                            targets: vec![(Const::Bool(true), BlockId(1))],
                            otherwise: BlockId(2),
                        },
                        fib,
                    ),
                ),
                block(
                    vec![
                        assign(
                            local(3),
                            Rvalue::Use(Operand::Copy(Place {
                                local: LocalId(2),
                                projections: vec![Projection::Deref, Projection::Field(0)],
                            })),
                            x,
                        ),
                        assign(local(0), Rvalue::Use(Operand::Move(local(3))), x),
                    ],
                    end(TerminatorKind::Return, fib),
                ),
                block(
                    vec![],
                    end(
                        TerminatorKind::Call {
                            callee: callee.clone(),
                            args: vec![copy(1), int_const(1)],
                            dest: Some(local(0)),
                            next: Some(BlockId(3)),
                        },
                        n,
                    ),
                ),
                block(vec![], end(TerminatorKind::Goto(BlockId(1)), fib)),
                block(vec![], end(TerminatorKind::Unreachable, fib)),
                block(
                    vec![],
                    end(
                        TerminatorKind::Call {
                            callee: Callee::External(vec!["Console".into(), "WriteLine".into()]),
                            args: vec![copy(1)],
                            dest: None,
                            next: Some(BlockId(1)),
                        },
                        fib,
                    ),
                ),
            ],
        )
    }

    #[test]
    fn dump_of_hand_built_body() {
        let fx = fixture();
        let expected = "\
fn Fib(n#0: int, this: in P) -> int {
    let _0: int;
    let n#0: int;
    let this: in P;
    let mut acc#1: int;
    let _4: bool;

    bb0: {
        storage_live(_4); // 6:5
        _4 = copy n#0 < 2; // 6:5
        switch copy _4 [true -> bb1] otherwise bb2; // 6:1
    }

    bb1: {
        acc#1 = copy (*this).X; // 2:12
        _0 = move acc#1; // 2:12
        return; // 6:1
    }

    bb2: {
        _0 = call UsersApi.Fib{fn UsersApi.Fib}(copy n#0, 1) -> bb3; // 6:5
    }

    bb3: {
        goto bb1; // 6:1
    }

    bb5: {
        call Console.WriteLine{external}(copy n#0) -> bb1; // 6:1
    }
}
";
        assert_eq!(dump_in(&fx, &hand_built(&fx)), expected);
    }

    #[test]
    fn dump_omits_empty_unreachable_blocks() {
        let fx = fixture();
        let text = dump_in(&fx, &hand_built(&fx));
        assert!(!text.contains("bb4"), "{text}");
        assert!(!text.contains("unreachable"), "{text}");
        // A block that is unreachable but holds a statement stays.
        let kept = one_block(
            &fx,
            vec![Statement {
                kind: StatementKind::Nop,
                span: nowhere(),
            }],
        );
        let mut kept = kept;
        kept.blocks[0].terminator.kind = TerminatorKind::Unreachable;
        let text = dump(&fx, &kept);
        assert!(text.contains("nop; // ?"), "{text}");
        assert!(text.contains("unreachable; // ?"), "{text}");
        // An empty block that does not end in unreachable stays too.
        let empty = one_block(&fx, vec![]);
        assert!(dump(&fx, &empty).contains("bb0: {\n        return; // ?"));
    }

    #[test]
    fn dump_is_byte_identical_across_runs() {
        let run = || {
            let fx = fixture();
            let mut locals = vec![temp(Ty::UNIT)];
            locals.extend((1..31).map(|n| {
                if n % 3 == 0 {
                    named(
                        &fx,
                        &format!("v{n}"),
                        (n / 3 % 3) as usize,
                        LocalTy::Value(int()),
                        n % 2 == 1,
                    )
                } else {
                    temp(int())
                }
            }));
            let statements = (1..31)
                .map(|n| {
                    assign(
                        local(n),
                        Rvalue::Binary(BinOp::Add, copy(n % 30 + 1), int_const(i128::from(n))),
                        at(&fx, "Fib"),
                    )
                })
                .collect();
            let body = body_of(
                "Many",
                0,
                locals,
                vec![block(statements, end(TerminatorKind::Return, nowhere()))],
            );
            dump_in(&fx, &body)
        };
        let first = run();
        assert_eq!(first, run());
        assert!(first.contains("let mut v3#1: int;"), "{first}");
        assert!(first.contains("let v6#2: int;"), "{first}");
        assert_eq!(first.matches(" + ").count(), 30);
    }

    #[test]
    fn spans_print_one_based_and_unknown_ones_as_a_question_mark() {
        let fx = fixture();
        let stmt = |span| assign(local(1), Rvalue::Use(int_const(1)), span);
        let start = Span::new(fx.file.id(&fx.db), TextRange::empty(TextSize::new(0)));
        let past_end = Span::new(
            fx.file.id(&fx.db),
            TextRange::empty(TextSize::new(SOURCE.len() as u32 + 5)),
        );
        let other_file = Span::new(
            FileId::new(fx.file.id(&fx.db).raw() + 1),
            TextRange::empty(TextSize::new(0)),
        );
        let body = one_block(
            &fx,
            vec![
                stmt(start),
                stmt(at(&fx, "Y: bool")),
                stmt(past_end),
                stmt(other_file),
            ],
        );
        let text = dump_in(&fx, &body);
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("_1 =")).collect();
        assert_eq!(
            lines,
            [
                "        _1 = 1; // 1:1",
                "        _1 = 1; // 3:12",
                "        _1 = 1; // ?",
                "        _1 = 1; // ?",
            ]
        );
        // Without a file every span is unknown.
        let text = dump(&fx, &body);
        assert_eq!(text.matches("// ?").count(), 5, "{text}");
    }

    #[test]
    fn constants_print_as_written() {
        let fx = fixture();
        let text = |constant| rvalue_text(&fx, Rvalue::Use(Operand::Const(constant)));
        assert_eq!(text(Const::Int(-3, Primitive::Long)), "-3");
        assert_eq!(text(Const::Int(7, Primitive::Byte)), "7");
        assert_eq!(
            text(Const::Real(1.5f64.to_bits(), Primitive::Double)),
            "1.5"
        );
        assert_eq!(text(Const::Real(3f64.to_bits(), Primitive::Float)), "3.0");
        assert_eq!(
            text(Const::Real(0.25f64.to_bits(), Primitive::Double)),
            "0.25"
        );
        assert_eq!(
            text(Const::Real((-0.0f64).to_bits(), Primitive::Double)),
            "-0.0"
        );
        assert_eq!(
            text(Const::Real(f64::NAN.to_bits(), Primitive::Double)),
            "NaN"
        );
        assert_eq!(
            text(Const::Real(f64::INFINITY.to_bits(), Primitive::Double)),
            "inf"
        );
        assert_eq!(
            text(Const::Real(1e21f64.to_bits(), Primitive::Double)),
            "1000000000000000000000.0"
        );
        let tenth = f64::from(0.1f32).to_bits();
        assert_eq!(text(Const::Real(tenth, Primitive::Float)), "0.1");
        assert_eq!(
            text(Const::Real(tenth, Primitive::Double)),
            "0.10000000149011612"
        );
        assert_eq!(text(Const::Bool(true)), "true");
        assert_eq!(text(Const::Bool(false)), "false");
        assert_eq!(text(Const::Unit), "()");
        assert_eq!(text(Const::Char('a')), "'a'");
        assert_eq!(text(Const::Char('\'')), "'\\''");
        assert_eq!(text(Const::Char('"')), "'\"'");
        assert_eq!(text(Const::Char('\n')), "'\\n'");
        assert_eq!(text(Const::String("text".into())), "\"text\"");
        assert_eq!(
            text(Const::String("a\"b\\c\nd\re\tf\0g\u{1}é".into())),
            "\"a\\\"b\\\\c\\nd\\re\\tf\\0g\\u{1}é\""
        );
        assert_eq!(text(Const::String("it's".into())), "\"it's\"");
    }

    #[test]
    fn invisible_characters_are_escaped() {
        let fx = fixture();
        let text = |value: &str| {
            rvalue_text(
                &fx,
                Rvalue::Use(Operand::Const(Const::String(value.into()))),
            )
        };
        assert_eq!(text("a\u{200b}b"), "\"a\\u{200b}b\"");
        assert_eq!(
            text("\u{a0}\u{202e}\u{feff}\u{2028}\u{3000}\u{ad}"),
            "\"\\u{a0}\\u{202e}\\u{feff}\\u{2028}\\u{3000}\\u{ad}\""
        );
        assert_eq!(text("é ü 日本"), "\"é ü 日本\"");
        assert_eq!(
            rvalue_text(&fx, Rvalue::Use(Operand::Const(Const::Char('\u{200b}')))),
            "'\\u{200b}'"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Interpolate(vec![InterpPiece::Text("\u{200b}".into())])
            ),
            "$\"\\u{200b}\""
        );
    }

    #[test]
    fn columns_count_characters_as_diagnostics_do() {
        let db = TestDb::new();
        let text = "é\u{1F600}x = 1;\nüüy\n";
        let file = db.file("src/Program.mr", text);
        let at = |needle: &str| {
            let start = text.find(needle).expect("the needle") as u32;
            Span::new(file.id(&db), TextRange::empty(TextSize::new(start)))
        };
        let stmt = |span| assign(local(1), Rvalue::Use(int_const(1)), span);
        let mut locals = vec![temp(Ty::UNIT)];
        locals.push(temp(int()));
        let body = body_of(
            "F",
            0,
            locals,
            vec![block(
                vec![stmt(at("x")), stmt(at("y"))],
                end(TerminatorKind::Return, at("é")),
            )],
        );
        let mut out = String::new();
        write_body_in(&mut out, &db, file, &body);
        assert!(out.contains("_1 = 1; // 1:3\n"), "{out}");
        assert!(out.contains("_1 = 1; // 2:3\n"), "{out}");
        assert!(out.contains("return; // 1:1\n"), "{out}");
    }

    #[test]
    fn every_operator_has_its_symbol() {
        let fx = fixture();
        let binary = [
            (BinOp::Add, "+"),
            (BinOp::Sub, "-"),
            (BinOp::Mul, "*"),
            (BinOp::Div, "/"),
            (BinOp::Rem, "%"),
            (BinOp::WrappingAdd, "+%"),
            (BinOp::WrappingSub, "-%"),
            (BinOp::WrappingMul, "*%"),
            (BinOp::Shl, "<<"),
            (BinOp::Shr, ">>"),
            (BinOp::UShr, ">>>"),
            (BinOp::BitAnd, "&"),
            (BinOp::BitOr, "|"),
            (BinOp::BitXor, "^"),
            (BinOp::Eq, "=="),
            (BinOp::Ne, "!="),
            (BinOp::Lt, "<"),
            (BinOp::Le, "<="),
            (BinOp::Gt, ">"),
            (BinOp::Ge, ">="),
        ];
        for (op, symbol) in binary {
            assert_eq!(
                rvalue_text(&fx, Rvalue::Binary(op, copy(2), int_const(1))),
                format!("copy _2 {symbol} 1")
            );
        }
        let unary = [(UnOp::Neg, "-"), (UnOp::Not, "!"), (UnOp::BitNot, "~")];
        for (op, symbol) in unary {
            assert_eq!(
                rvalue_text(&fx, Rvalue::Unary(op, Operand::Move(local(2)))),
                format!("{symbol}move _2")
            );
        }
    }

    #[test]
    fn rvalues_print_their_parts() {
        let fx = fixture();
        assert_eq!(
            rvalue_text(&fx, Rvalue::Borrow(BorrowMode::In, local(2))),
            "&in _2"
        );
        assert_eq!(
            rvalue_text(&fx, Rvalue::Borrow(BorrowMode::Ref, local(2))),
            "&ref _2"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Cast(CastKind::Implicit, copy(2), Ty::Primitive(Primitive::Long))
            ),
            "(implicit long)copy _2"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Cast(CastKind::Numeric, copy(2), Ty::Primitive(Primitive::Byte))
            ),
            "(byte)copy _2"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Aggregate(AggregateKind::Struct(fx.p), vec![copy(2), int_const(1)])
            ),
            "UsersApi.P{struct UsersApi.P}(copy _2, 1)"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Aggregate(AggregateKind::Tuple, vec![copy(2), int_const(1)])
            ),
            "(copy _2, 1)"
        );
        assert_eq!(
            rvalue_text(&fx, Rvalue::Aggregate(AggregateKind::Tuple, vec![copy(2)])),
            "(copy _2,)"
        );
        assert_eq!(
            rvalue_text(&fx, Rvalue::Aggregate(AggregateKind::Tuple, vec![])),
            "()"
        );
        assert_eq!(rvalue_text(&fx, Rvalue::Interpolate(vec![])), "$\"\"");
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Interpolate(vec![
                    InterpPiece::Text("x = {\"".into()),
                    InterpPiece::Operand {
                        value: copy(2),
                        alignment: None,
                        format: None,
                    },
                    InterpPiece::Text("}\n".into()),
                ])
            ),
            "$\"x = {{\\\"{copy _2}}}\\n\""
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Interpolate(vec![
                    InterpPiece::Operand {
                        value: copy(2),
                        alignment: Some(int_const(-5)),
                        format: Some("x4".into()),
                    },
                    InterpPiece::Operand {
                        value: copy(2),
                        alignment: Some(copy(1)),
                        format: None,
                    },
                    InterpPiece::Operand {
                        value: copy(2),
                        alignment: None,
                        format: Some("F2".into()),
                    },
                ])
            ),
            "$\"{copy _2,-5:x4}{copy _2,copy _1}{copy _2:F2}\""
        );
    }

    #[test]
    fn statements_print_their_kind() {
        let fx = fixture();
        let body = one_block(
            &fx,
            vec![
                Statement {
                    kind: StatementKind::StorageLive(LocalId(2)),
                    span: nowhere(),
                },
                Statement {
                    kind: StatementKind::StorageDead(LocalId(2)),
                    span: nowhere(),
                },
                Statement {
                    kind: StatementKind::Nop,
                    span: nowhere(),
                },
            ],
        );
        let text = dump(&fx, &body);
        assert!(
            text.contains("        storage_live(_2); // ?\n        storage_dead(_2); // ?\n        nop; // ?\n"),
            "{text}"
        );
    }

    #[test]
    fn places_print_fields_by_name_or_index_and_derefs() {
        let fx = fixture();
        let pair = Ty::Tuple(vec![
            TupleElem {
                name: None,
                ty: Ty::Struct(fx.p),
            },
            TupleElem {
                name: None,
                ty: int(),
            },
        ]);
        let mut locals = vec![temp(Ty::UNIT)];
        locals.push(named(&fx, "x", 0, LocalTy::Value(Ty::Struct(fx.p)), false));
        locals.push(named(&fx, "t", 1, LocalTy::Value(pair.clone()), false));
        locals.push(named(
            &fx,
            "r",
            2,
            LocalTy::Borrow(BorrowMode::Ref, pair),
            false,
        ));
        locals.push(named(&fx, "i", 3, LocalTy::Value(int()), false));
        let place = |local: u32, projections: &[Projection]| Place {
            local: LocalId(local),
            projections: projections.to_vec(),
        };
        let places = [
            (place(1, &[Projection::Field(0)]), "x#0.X"),
            (place(1, &[Projection::Field(1)]), "x#0.Y"),
            (place(2, &[Projection::Field(1)]), "t#1.1"),
            (
                place(2, &[Projection::Field(0), Projection::Field(1)]),
                "t#1.0.Y",
            ),
            (place(3, &[Projection::Deref]), "(*r#2)"),
            (
                place(
                    3,
                    &[
                        Projection::Deref,
                        Projection::Field(0),
                        Projection::Field(0),
                    ],
                ),
                "(*r#2).0.X",
            ),
            (place(1, &[Projection::Field(9)]), "x#0.9"),
            (place(4, &[Projection::Field(0)]), "i#3.0"),
            (
                place(4, &[Projection::Field(0), Projection::Field(0)]),
                "i#3.0.0",
            ),
        ];
        let statements = places
            .iter()
            .map(|(p, _)| assign(local(0), Rvalue::Use(Operand::Copy(p.clone())), nowhere()))
            .collect();
        let body = body_of(
            "F",
            0,
            locals,
            vec![block(statements, end(TerminatorKind::Return, nowhere()))],
        );
        let text = dump(&fx, &body);
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("_0 =")).collect();
        let expected: Vec<String> = places
            .iter()
            .map(|(_, text)| format!("        _0 = copy {text}; // ?"))
            .collect();
        assert_eq!(lines, expected);
    }

    #[test]
    fn terminators_print_their_targets() {
        let fx = fixture();
        let term = |kind| {
            let mut body = one_block(&fx, vec![]);
            body.blocks[0].terminator = end(kind, nowhere());
            let text = dump(&fx, &body);
            text.lines()
                .find(|l| l.starts_with("        ") && l.ends_with("// ?"))
                .expect("the terminator")
                .trim()
                .strip_suffix(" // ?")
                .expect("the span")
                .to_owned()
        };
        assert_eq!(term(TerminatorKind::Goto(BlockId(2))), "goto bb2;");
        assert_eq!(term(TerminatorKind::Return), "return;");
        assert_eq!(
            term(TerminatorKind::Switch {
                discr: copy(1),
                targets: vec![],
                otherwise: BlockId(4),
            }),
            "switch copy _1 [] otherwise bb4;"
        );
        assert_eq!(
            term(TerminatorKind::Switch {
                discr: copy(1),
                targets: vec![
                    (Const::Int(0, Primitive::Int), BlockId(1)),
                    (Const::Int(1, Primitive::Int), BlockId(2)),
                ],
                otherwise: BlockId(3),
            }),
            "switch copy _1 [0 -> bb1, 1 -> bb2] otherwise bb3;"
        );
        let call = |callee, dest, next| TerminatorKind::Call {
            callee,
            args: vec![copy(1), Operand::Move(local(2))],
            dest,
            next,
        };
        assert_eq!(
            term(call(Callee::Local(2), None, None)),
            "call local#2(copy _1, move _2) -> !;"
        );
        assert_eq!(
            term(call(Callee::Local(0), Some(local(3)), Some(BlockId(1)))),
            "_3 = call local#0(copy _1, move _2) -> bb1;"
        );
        assert_eq!(
            term(call(Callee::Initializer(fx.p, 1), None, Some(BlockId(1)))),
            "call UsersApi.P{struct UsersApi.P}.Y{init}(copy _1, move _2) -> bb1;"
        );
        assert_eq!(
            term(call(Callee::Initializer(fx.p, 7), None, Some(BlockId(1)))),
            "call UsersApi.P{struct UsersApi.P}.7{init}(copy _1, move _2) -> bb1;"
        );
        assert_eq!(
            term(call(Callee::Initializer(fx.fib, 0), None, Some(BlockId(1)))),
            "call UsersApi.Fib{fn UsersApi.Fib}.0{init}(copy _1, move _2) -> bb1;"
        );
    }

    #[test]
    fn a_local_function_callee_prints_by_the_name_of_its_body() {
        let fx = fixture();
        let callee_body = |index: usize| {
            let mut body = one_block(&fx, vec![]);
            body.blocks[0].terminator = end(
                TerminatorKind::Call {
                    callee: Callee::Local(index),
                    args: vec![],
                    dest: None,
                    next: Some(BlockId(0)),
                },
                nowhere(),
            );
            body
        };
        let mir = Mir {
            bodies: vec![
                callee_body(1),
                body_of("Fib.Step", 0, vec![temp(Ty::UNIT)], vec![]),
            ],
        };
        let mut out = String::new();
        write_mir(&mut out, &fx.db, Some(fx.file), &mir);
        assert!(out.contains("call Fib.Step() -> bb0; // ?"), "{out}");
        // The bodies are separated by one blank line.
        assert!(out.starts_with("fn F() -> () {"), "{out}");
        assert!(out.contains("}\n\nfn Fib.Step() -> () {"), "{out}");
        // An index with no body keeps the index.
        let mut out = String::new();
        write_mir(
            &mut out,
            &fx.db,
            None,
            &Mir {
                bodies: vec![callee_body(5)],
            },
        );
        assert!(out.contains("call local#5() -> bb0;"), "{out}");
    }

    #[test]
    fn locals_print_their_types() {
        let fx = fixture();
        let body = body_of(
            "G",
            1,
            vec![
                temp(Ty::UNIT),
                named(&fx, "r", 3, LocalTy::Borrow(BorrowMode::Ref, int()), false),
            ],
            vec![],
        );
        assert_eq!(
            dump(&fx, &body),
            "fn G(r#3: ref int) -> () {\n    let _0: ();\n    let r#3: ref int;\n}\n"
        );
        let empty = body_of("H", 0, vec![temp(int())], vec![]);
        assert_eq!(dump(&fx, &empty), "fn H() -> int {\n    let _0: int;\n}\n");
    }

    #[test]
    fn write_body_appends() {
        let fx = fixture();
        let body = body_of("H", 0, vec![temp(int())], vec![]);
        let mut out = String::from("before\n");
        write_body(&mut out, &fx.db, &body);
        assert!(out.starts_with("before\nfn H()"), "{out}");
    }
}
