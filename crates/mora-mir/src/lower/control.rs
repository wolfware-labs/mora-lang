//! Control flow: `if`, the loops, `break` and `continue`, and the
//! operators that branch: `&&`, `||` and `c ? a : b`.
//!
//! A branch on a `bool` is `switch` with `false` as its one target and the
//! block where the value is `true` as `otherwise`. The temporaries of a
//! condition live until the `switch` and end at the start of both blocks
//! it goes to. A loop whose condition is the literal `true`, or that has
//! none, has no `switch`: only `break` leaves it (D-138).

use mora_hir::{ExprId, StmtId, is_literal_bool};
use mora_span::{Span, TextRange};

use super::Lowerer;
use crate::{BlockId, Const, LocalTy, Operand, Place, Rvalue, TerminatorKind};

/// A loop around the statement being lowered.
pub(super) struct Loop {
    /// The block after the loop, where `break` goes.
    r#break: BlockId,
    /// Where `continue` goes: the condition, or the update of a `for`.
    r#continue: BlockId,
    /// How many scopes are open outside the loop's body: `break` and
    /// `continue` end the locals of the scopes opened after them.
    depth: usize,
}

impl Lowerer<'_> {
    /// `if (condition) then else r#else`.
    pub(super) fn r#if(
        &mut self,
        stmt: StmtId,
        condition: ExprId,
        then: StmtId,
        r#else: Option<StmtId>,
    ) {
        self.branches(
            condition,
            self.stmt_span(stmt),
            |this| this.stmt(then),
            |this| {
                if let Some(r#else) = r#else {
                    this.stmt(r#else);
                }
            },
        );
    }

    /// `while (condition) body`: the condition, then the body, which goes
    /// back to the condition.
    pub(super) fn r#while(&mut self, stmt: StmtId, condition: ExprId, body: StmtId) {
        let span = self.stmt_span(stmt);
        let head = self.builder.new_block();
        self.builder.goto(head, span);
        self.builder.switch_to(head);
        let (start, exit) = self.loop_condition(Some(condition), head);
        let after = self.builder.new_block();
        self.loop_body(body, after, head, start, span);
        self.leave_loop(exit, after, span);
    }

    /// `do body while (condition);`: the body, then the condition, which
    /// goes back to the body. A condition that is the literal `false` does
    /// not go back.
    pub(super) fn do_while(&mut self, stmt: StmtId, body: StmtId, condition: ExprId) {
        let span = self.stmt_span(stmt);
        let start = self.builder.new_block();
        self.builder.goto(start, span);
        let next = self.builder.new_block();
        let after = self.builder.new_block();
        self.loop_body(body, after, next, start, span);
        self.builder.switch_to(next);
        if is_literal_bool(self.hir, condition, false) {
            self.builder.goto(after, span);
        } else {
            let (back, exit) = self.loop_condition(Some(condition), next);
            self.builder.switch_to(back);
            self.builder.goto(start, span);
            self.leave_loop(exit, after, span);
        }
        self.builder.switch_to(after);
    }

    /// `for (init; condition; update) body`: the locals of `init` live to
    /// the end of the loop, and `continue` goes to the update.
    pub(super) fn r#for(
        &mut self,
        stmt: StmtId,
        init: &[StmtId],
        condition: Option<ExprId>,
        update: &[ExprId],
        body: StmtId,
    ) {
        let span = self.stmt_span(stmt);
        self.builder.enter_scope();
        self.stmts(init);
        let head = self.builder.new_block();
        self.builder.goto(head, span);
        self.builder.switch_to(head);
        let (start, exit) = self.loop_condition(condition, head);
        let after = self.builder.new_block();
        let next = self.builder.new_block();
        self.loop_body(body, after, next, start, span);
        self.builder.switch_to(next);
        for &expr in update {
            let mark = self.builder.temp_mark();
            self.effect(expr);
            self.builder.end_temps(mark, self.span(expr));
        }
        self.builder.goto(head, span);
        self.leave_loop(exit, after, span);
        let end = self.stmt_end(body);
        self.builder.leave_scope(end);
    }

    /// `break` or `continue`: the storage of the locals of every scope
    /// opened inside the loop ends, the innermost first, and control goes to
    /// the loop's target. The scopes stay open for the code after it.
    pub(super) fn loop_jump(&mut self, stmt: StmtId, is_break: bool) {
        let span = self.stmt_span(stmt);
        let Some(target) = self.loops.last() else {
            self.ice_at(span.range, "`break` or `continue` outside a loop")
        };
        let to = if is_break {
            target.r#break
        } else {
            target.r#continue
        };
        let depth = target.depth;
        // A jump is a statement: no temporary is open.
        self.builder
            .exit_scopes(depth, self.builder.temp_mark(), span);
        self.builder.jump(TerminatorKind::Goto(to), span);
    }

    /// `lhs && rhs` (`and`) or `lhs || rhs` in a new temporary: `rhs` is
    /// evaluated only where `lhs` does not decide the value.
    pub(super) fn logical(&mut self, expr: ExprId, and: bool, lhs: ExprId, rhs: ExprId) -> Operand {
        let span = self.span(expr);
        let ty = self.ty(expr);
        let result = Place::local(self.builder.temp(LocalTy::Value(ty), span));
        let decided = |this: &mut Self| {
            let value = Rvalue::Use(Operand::Const(Const::Bool(!and)));
            this.builder.assign(result.clone(), value, span);
        };
        let rest = |this: &mut Self| this.value_into(rhs, result.clone());
        if and {
            self.branches(lhs, span, rest, decided);
        } else {
            self.branches(lhs, span, decided, rest);
        }
        Operand::Copy(result)
    }

    /// `condition ? then : r#else` in a new temporary.
    pub(super) fn conditional(
        &mut self,
        expr: ExprId,
        condition: ExprId,
        then: ExprId,
        r#else: ExprId,
    ) -> Operand {
        let span = self.span(expr);
        let ty = self.ty(expr);
        let result = Place::local(self.builder.temp(LocalTy::Value(ty), span));
        self.branches(
            condition,
            span,
            |this| this.value_into(then, result.clone()),
            |this| this.value_into(r#else, result.clone()),
        );
        Operand::Copy(result)
    }

    /// Stores the value of `expr` in `dest` inside a branch, whose own
    /// temporaries end there.
    fn value_into(&mut self, expr: ExprId, dest: Place) {
        let mark = self.builder.temp_mark();
        self.expr_into(expr, dest);
        self.builder.end_temps(mark, self.span(expr));
    }

    /// `then` where `condition` is true and `r#else` where it is false; both
    /// go on in the block after them.
    fn branches(
        &mut self,
        condition: ExprId,
        span: Span,
        then: impl FnOnce(&mut Self),
        r#else: impl FnOnce(&mut Self),
    ) {
        let (when_true, when_false) = self.branch(condition);
        self.builder.switch_to(when_true);
        then(self);
        let join = self.builder.new_block();
        self.builder.goto(join, span);
        self.builder.switch_to(when_false);
        r#else(self);
        self.builder.goto(join, span);
        self.builder.switch_to(join);
    }

    /// Evaluates `condition` and switches on it. Returns the block where it
    /// is true and the block where it is false, each of which first ends
    /// the condition's temporaries.
    fn branch(&mut self, condition: ExprId) -> (BlockId, BlockId) {
        let span = self.span(condition);
        let mark = self.builder.temp_mark();
        let value = self.operand(condition);
        let temps = self.builder.take_temps(mark);
        let when_true = self.builder.new_block();
        let when_false = self.builder.new_block();
        self.builder.terminate(
            TerminatorKind::Switch {
                discr: value,
                targets: vec![(Const::Bool(false), when_false)],
                otherwise: when_true,
            },
            span,
        );
        for block in [when_true, when_false] {
            self.builder.switch_to(block);
            self.builder.end_storage(&temps, span);
        }
        (when_true, when_false)
    }

    /// The condition of a loop, lowered in the current block `head`.
    /// Returns where the body starts and, unless only `break` leaves the
    /// loop, the block where the condition is false.
    fn loop_condition(
        &mut self,
        condition: Option<ExprId>,
        head: BlockId,
    ) -> (BlockId, Option<BlockId>) {
        match condition {
            Some(condition) if !is_literal_bool(self.hir, condition, true) => {
                let (when_true, when_false) = self.branch(condition);
                (when_true, Some(when_false))
            }
            _ => (head, None),
        }
    }

    /// The body of a loop, starting in `start`, with `break` going to
    /// `after` and `continue` to `next`, where the end of the body goes too.
    fn loop_body(
        &mut self,
        body: StmtId,
        after: BlockId,
        next: BlockId,
        start: BlockId,
        span: Span,
    ) {
        self.loops.push(Loop {
            r#break: after,
            r#continue: next,
            depth: self.builder.scope_depth(),
        });
        self.builder.switch_to(start);
        self.stmt(body);
        self.loops.pop();
        self.builder.goto(next, span);
    }

    /// Goes on in `after`, which the block `exit`, where the condition is
    /// false, goes to.
    fn leave_loop(&mut self, exit: Option<BlockId>, after: BlockId, span: Span) {
        if let Some(exit) = exit {
            self.builder.switch_to(exit);
            self.builder.goto(after, span);
        }
        self.builder.switch_to(after);
    }

    /// Where the statement `stmt` ends: the closing brace of a block, or the
    /// end of its text.
    fn stmt_end(&self, stmt: StmtId) -> Span {
        if let Some(&brace) = self.map.block_ends.get(&stmt) {
            return Span::new(self.file.id(self.db), brace);
        }
        let span = self.stmt_span(stmt);
        Span::new(span.file, TextRange::empty(span.range.end()))
    }
}
