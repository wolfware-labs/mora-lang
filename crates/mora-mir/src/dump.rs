//! The text form of MIR bodies, for snapshot tests and `--emit mir`.
//!
//! It sits next to the HIR dump (`mora_hir::hir_dump`) and follows its
//! conventions: a resolved item is `Name{kind path}`, as in
//! `App.Fib{fn App.Fib}`, a standard-library function is
//! `Console.WriteLine{external}`, and a type is written as code writes it.
//! A local with a source name is `name#n` and any other is `_n`, where `n`
//! is the local's index in the body, so `_0` is the return slot and the
//! parameters are `_1` to the last one. The output is a pure function of
//! the body and the database: no map is iterated, so the same input gives
//! the same bytes.
//!
//! # Layout
//!
//! A body is a header, one line per local, and its blocks:
//!
//! ```text
//! fn Fib(n#1: int) -> int {
//!     let _0: int;
//!     let n#1: int;
//!     let mut acc#2: int;
//!     let _3: bool;
//!
//!     bb0: {
//!         _3 = lt(copy n#1, 2); // 3:9
//!         switch copy _3 [true -> bb1] otherwise bb2; // 3:5
//!     }
//!
//!     bb1: {
//!         _0 = copy n#1; // 4:16
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
//! A block with no statements that ends in `unreachable` is left out; the
//! other blocks keep their numbers. Every statement and terminator ends in
//! `// line:col`, the one-based line and the one-based column in bytes where
//! its source starts, or `// ?` when the span names no position in the
//! file (see [`write_body_in`]).
//!
//! # Statements
//!
//! * `place = rvalue;`, `storage_live(local);`, `storage_dead(local);` and
//!   `nop;`.
//!
//! # Places
//!
//! A place is its local followed by its projections. A field of a struct is
//! `x#1.X`, by name, and a field of a tuple is `t#2.0`, by index. A
//! dereference is `(*this#1)`, so a field behind a borrow is
//! `(*this#1).X`.
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
//! * `add(a, b)`: a binary operation, named `add`, `sub`, `mul`, `div`,
//!   `rem`, `wrapping_add`, `wrapping_sub`, `wrapping_mul`, `shl`, `shr`,
//!   `ushr`, `bit_and`, `bit_or`, `bit_xor`, `eq`, `ne`, `lt`, `le`, `gt` or
//!   `ge`.
//! * `neg(a)`, `not(a)` and `bit_not(a)`: a unary operation.
//! * `cast_implicit(a, type)` and `cast_numeric(a, type)`: a conversion.
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
//!   not return. The callee is an item as above, `local#n` for the `n`-th
//!   body of the MIR, `Item{kind path}.X{init}` for the initializer of a
//!   field (by index when the item has no such field), or the path of a
//!   standard-library function with `{external}`.

use std::fmt::Write;

use mora_hir::{Signature, Ty, item_text, signature, ty_name};
use mora_span::{Db, FileId, LineIndex, SourceFile, line_index};

use crate::{
    AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const, InterpPiece,
    LocalId, LocalTy, Operand, Place, Projection, Rvalue, Statement, StatementKind, Terminator,
    TerminatorKind, UnOp,
};

/// Appends the text of `body` to `out`. No file is known, so every span
/// prints as `// ?`; [`write_body_in`] prints the positions.
pub fn write_body(out: &mut String, db: &dyn Db, body: &Body) {
    Printer {
        db,
        body,
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
        lines: Some((file.id(db), line_index(db, file))),
        out,
    }
    .body();
}

struct Printer<'a> {
    db: &'a dyn Db,
    body: &'a Body,
    lines: Option<(FileId, &'a LineIndex)>,
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
            Some(source) => format!("{}#{}", source.name, id.0),
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
        let position = self.lines.and_then(|(file, index)| {
            if span.file != file {
                return None;
            }
            index.line_col(span.range.start())
        });
        match position {
            Some(position) => {
                let _ = writeln!(self.out, " // {}:{}", position.line + 1, position.col + 1);
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
            Callee::Local(index) => format!("local#{index}"),
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
                "{}({}, {})",
                binary_name(*op),
                self.operand(left),
                self.operand(right)
            ),
            Rvalue::Unary(op, operand) => {
                format!("{}({})", unary_name(*op), self.operand(operand))
            }
            Rvalue::Cast(kind, operand, ty) => {
                let kind = match kind {
                    CastKind::Implicit => "implicit",
                    CastKind::Numeric => "numeric",
                };
                format!(
                    "cast_{kind}({}, {})",
                    self.operand(operand),
                    ty_name(self.db, ty)
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
                                    _ => escape(ch, '"', &mut text),
                                }
                            }
                        }
                        InterpPiece::Operand(operand) => {
                            let _ = write!(text, "{{{}}}", self.operand(operand));
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

fn binary_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::Rem => "rem",
        BinOp::WrappingAdd => "wrapping_add",
        BinOp::WrappingSub => "wrapping_sub",
        BinOp::WrappingMul => "wrapping_mul",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::UShr => "ushr",
        BinOp::BitAnd => "bit_and",
        BinOp::BitOr => "bit_or",
        BinOp::BitXor => "bit_xor",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
    }
}

fn unary_name(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
        UnOp::BitNot => "bit_not",
    }
}

/// A constant as written.
fn constant(value: &Const) -> String {
    match value {
        Const::Int(value, _) => value.to_string(),
        Const::Real(bits, _) => format!("{:?}", f64::from_bits(*bits)),
        Const::Bool(value) => value.to_string(),
        Const::Char(ch) => {
            let mut text = String::from("'");
            escape(*ch, '\'', &mut text);
            text.push('\'');
            text
        }
        Const::String(value) => {
            let mut text = String::from("\"");
            for ch in value.chars() {
                escape(ch, '"', &mut text);
            }
            text.push('"');
            text
        }
        Const::Unit => "()".to_owned(),
    }
}

/// `ch` as it is written inside quotes `quote`.
fn escape(ch: char, quote: char, out: &mut String) {
    match ch {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\0' => out.push_str("\\0"),
        _ if ch == quote => {
            out.push('\\');
            out.push(ch);
        }
        _ if ch.is_control() => {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
        }
        _ => out.push(ch),
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
        LocalDecl, LocalId, LocalSource, LocalTy, Operand, Place, Projection, Rvalue, Statement,
        StatementKind, Terminator, TerminatorKind, UnOp,
    };

    const SOURCE: &str =
        "struct P {\n    public X: int;\n    public Y: bool;\n}\n\nFib(n: int): int => n;\n";

    struct Fx {
        db: TestDb,
        file: SourceFile,
        p: ItemId,
        fib: ItemId,
        hir_local: mora_hir::LocalId,
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
        let hir_local = body(&db, file, BodyOwner::Item(fib.item)).params[0];
        Fx {
            db,
            file,
            p,
            fib,
            hir_local,
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

    fn named(fx: &Fx, name: &str, ty: LocalTy, mutable: bool) -> LocalDecl {
        LocalDecl {
            ty,
            mutable,
            source: Some(LocalSource {
                name: name.into(),
                hir: fx.hir_local,
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
                named(fx, "n", LocalTy::Value(int()), false),
                named(fx, "this", LocalTy::Borrow(BorrowMode::In, p), false),
                named(fx, "acc", LocalTy::Value(int()), true),
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
fn Fib(n#1: int, this#2: in P) -> int {
    let _0: int;
    let n#1: int;
    let this#2: in P;
    let mut acc#3: int;
    let _4: bool;

    bb0: {
        storage_live(_4); // 6:5
        _4 = lt(copy n#1, 2); // 6:5
        switch copy _4 [true -> bb1] otherwise bb2; // 6:1
    }

    bb1: {
        acc#3 = copy (*this#2).X; // 2:12
        _0 = move acc#3; // 2:12
        return; // 6:1
    }

    bb2: {
        _0 = call UsersApi.Fib{fn UsersApi.Fib}(copy n#1, 1) -> bb3; // 6:5
    }

    bb3: {
        goto bb1; // 6:1
    }

    bb5: {
        call Console.WriteLine{external}(copy n#1) -> bb1; // 6:1
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
                    named(&fx, &format!("v{n}"), LocalTy::Value(int()), n % 2 == 1)
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
        assert!(first.contains("let mut v3#3: int;"), "{first}");
        assert!(first.contains("let v6#6: int;"), "{first}");
        assert_eq!(first.matches("= add(").count(), 30);
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
    fn every_operator_has_its_name() {
        let fx = fixture();
        let binary = [
            (BinOp::Add, "add"),
            (BinOp::Sub, "sub"),
            (BinOp::Mul, "mul"),
            (BinOp::Div, "div"),
            (BinOp::Rem, "rem"),
            (BinOp::WrappingAdd, "wrapping_add"),
            (BinOp::WrappingSub, "wrapping_sub"),
            (BinOp::WrappingMul, "wrapping_mul"),
            (BinOp::Shl, "shl"),
            (BinOp::Shr, "shr"),
            (BinOp::UShr, "ushr"),
            (BinOp::BitAnd, "bit_and"),
            (BinOp::BitOr, "bit_or"),
            (BinOp::BitXor, "bit_xor"),
            (BinOp::Eq, "eq"),
            (BinOp::Ne, "ne"),
            (BinOp::Lt, "lt"),
            (BinOp::Le, "le"),
            (BinOp::Gt, "gt"),
            (BinOp::Ge, "ge"),
        ];
        for (op, name) in binary {
            assert_eq!(
                rvalue_text(&fx, Rvalue::Binary(op, copy(2), int_const(1))),
                format!("{name}(copy _2, 1)")
            );
        }
        let unary = [
            (UnOp::Neg, "neg"),
            (UnOp::Not, "not"),
            (UnOp::BitNot, "bit_not"),
        ];
        for (op, name) in unary {
            assert_eq!(
                rvalue_text(&fx, Rvalue::Unary(op, Operand::Move(local(2)))),
                format!("{name}(move _2)")
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
            "cast_implicit(copy _2, long)"
        );
        assert_eq!(
            rvalue_text(
                &fx,
                Rvalue::Cast(CastKind::Numeric, copy(2), Ty::Primitive(Primitive::Byte))
            ),
            "cast_numeric(copy _2, byte)"
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
                    InterpPiece::Operand(copy(2)),
                    InterpPiece::Text("}\n".into()),
                ])
            ),
            "$\"x = {{\\\"{copy _2}}}\\n\""
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
        locals.push(named(&fx, "x", LocalTy::Value(Ty::Struct(fx.p)), false));
        locals.push(named(&fx, "t", LocalTy::Value(pair.clone()), false));
        locals.push(named(
            &fx,
            "r",
            LocalTy::Borrow(BorrowMode::Ref, pair),
            false,
        ));
        locals.push(named(&fx, "i", LocalTy::Value(int()), false));
        let place = |local: u32, projections: &[Projection]| Place {
            local: LocalId(local),
            projections: projections.to_vec(),
        };
        let places = [
            (place(1, &[Projection::Field(0)]), "x#1.X"),
            (place(1, &[Projection::Field(1)]), "x#1.Y"),
            (place(2, &[Projection::Field(1)]), "t#2.1"),
            (
                place(2, &[Projection::Field(0), Projection::Field(1)]),
                "t#2.0.Y",
            ),
            (place(3, &[Projection::Deref]), "(*r#3)"),
            (
                place(
                    3,
                    &[
                        Projection::Deref,
                        Projection::Field(0),
                        Projection::Field(0),
                    ],
                ),
                "(*r#3).0.X",
            ),
            (place(1, &[Projection::Field(9)]), "x#1.9"),
            (place(4, &[Projection::Field(0)]), "i#4.0"),
            (
                place(4, &[Projection::Field(0), Projection::Field(0)]),
                "i#4.0.0",
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
    fn locals_print_their_types() {
        let fx = fixture();
        let body = body_of(
            "G",
            1,
            vec![
                temp(Ty::UNIT),
                named(&fx, "r", LocalTy::Borrow(BorrowMode::Ref, int()), false),
            ],
            vec![],
        );
        assert_eq!(
            dump(&fx, &body),
            "fn G(r#1: ref int) -> () {\n    let _0: ();\n    let r#1: ref int;\n}\n"
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
