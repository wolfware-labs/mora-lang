//! Control flow: `if`, the loops, `break` and `continue`, and the
//! operators that branch: `&&`, `||` and `c ? a : b`.
//!
//! A branch on a `bool` is `switch` with `false` as its one target and the
//! block where the value is `true` as `otherwise`. The temporaries of a
//! condition live until the `switch` and end at the start of both blocks
//! it goes to. A loop whose condition is the literal `true`, or that has
//! none, has no `switch`: only `break` leaves it (D-138). A condition that
//! calls a function that does not return ends the statement there.

use mora_hir::{ExprId, StmtId, is_literal_bool};
use mora_span::{Span, TextRange};

use super::{Diverged, Lowered, Lowerer};
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
        // A condition that does not return ends the statement.
        let _ = self.branches(
            condition,
            self.stmt_span(stmt),
            |this| {
                this.stmt(then);
                Ok(())
            },
            |this| {
                if let Some(r#else) = r#else {
                    this.stmt(r#else);
                }
                Ok(())
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
        let Ok((start, exit)) = self.loop_condition(Some(condition), head) else {
            return;
        };
        let after = self.builder.new_block();
        self.loop_body(body, after, head, start, span);
        self.leave_loop(exit, after, span);
    }

    /// `do body while (condition);`: the body, then the condition, which
    /// goes back to the body. A condition that is the literal `false` does
    /// not go back; one that control never gets to is not lowered.
    pub(super) fn do_while(&mut self, stmt: StmtId, body: StmtId, condition: ExprId) {
        let span = self.stmt_span(stmt);
        let start = self.builder.new_block();
        self.builder.goto(start, span);
        let next = self.builder.new_block();
        let after = self.builder.new_block();
        self.loop_body(body, after, next, start, span);
        self.builder.switch_to(next);
        if !self.builder.is_reachable() || is_literal_bool(self.hir, condition, false) {
            self.builder.goto(after, span);
        } else if let Ok((back, exit)) = self.loop_condition(Some(condition), next) {
            self.builder.switch_to(back);
            self.builder.goto(start, span);
            if let Some(exit) = exit {
                self.builder.switch_to(exit);
                self.builder.goto(after, span);
            }
        } else {
            // The condition does not return; `break` still gets to `after`.
            self.builder.goto(after, span);
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
        let end = self.stmt_end(body);
        self.builder.enter_scope();
        self.stmts(init);
        if self.builder.is_reachable() {
            self.for_loop(condition, update, body, span);
        }
        self.builder.leave_scope(end);
    }

    /// The loop of a `for`, after its `init`.
    fn for_loop(&mut self, condition: Option<ExprId>, update: &[ExprId], body: StmtId, span: Span) {
        let head = self.builder.new_block();
        self.builder.goto(head, span);
        self.builder.switch_to(head);
        let Ok((start, exit)) = self.loop_condition(condition, head) else {
            return;
        };
        let after = self.builder.new_block();
        let next = self.builder.new_block();
        self.loop_body(body, after, next, start, span);
        self.builder.switch_to(next);
        for &expr in update {
            if !self.builder.is_reachable() {
                break;
            }
            let mark = self.builder.temp_mark();
            let _ = self.effect(expr);
            self.builder.end_temps(mark, self.span(expr));
        }
        self.builder.goto(head, span);
        self.leave_loop(exit, after, span);
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
    pub(super) fn logical(
        &mut self,
        expr: ExprId,
        and: bool,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Lowered<Operand> {
        let span = self.span(expr);
        let ty = self.ty(expr);
        let result = Place::local(self.builder.temp(LocalTy::Value(ty), span));
        let decided = |this: &mut Self| {
            let value = Rvalue::Use(Operand::Const(Const::Bool(!and)));
            this.builder.assign(result.clone(), value, span);
            Ok(())
        };
        let rest = |this: &mut Self| this.value_into(rhs, result.clone());
        if and {
            self.branches(lhs, span, rest, decided)?;
        } else {
            self.branches(lhs, span, decided, rest)?;
        }
        Ok(Operand::Copy(result))
    }

    /// `condition ? then : r#else` in a new temporary.
    pub(super) fn conditional(
        &mut self,
        expr: ExprId,
        condition: ExprId,
        then: ExprId,
        r#else: ExprId,
    ) -> Lowered<Operand> {
        let span = self.span(expr);
        let ty = self.ty(expr);
        let result = Place::local(self.builder.temp(LocalTy::Value(ty), span));
        self.branches(
            condition,
            span,
            |this| this.value_into(then, result.clone()),
            |this| this.value_into(r#else, result.clone()),
        )?;
        Ok(Operand::Copy(result))
    }

    /// Stores the value of `expr` in `dest` inside a branch, whose own
    /// temporaries end there.
    fn value_into(&mut self, expr: ExprId, dest: Place) -> Lowered<()> {
        let mark = self.builder.temp_mark();
        let value = self.expr_into(expr, dest);
        self.builder.end_temps(mark, self.span(expr));
        value
    }

    /// `then` where `condition` is true and `r#else` where it is false; both
    /// go on in the block after them. [`Diverged`] when the condition does
    /// not return, or neither branch gets to that block.
    fn branches(
        &mut self,
        condition: ExprId,
        span: Span,
        then: impl FnOnce(&mut Self) -> Lowered<()>,
        r#else: impl FnOnce(&mut Self) -> Lowered<()>,
    ) -> Lowered<()> {
        let (when_true, when_false) = self.branch(condition)?;
        self.builder.switch_to(when_true);
        // A branch that does not return ends in a block control never gets
        // to, which the jump to the join turns into `unreachable`.
        let _ = then(self);
        let join = self.builder.new_block();
        self.builder.goto(join, span);
        self.builder.switch_to(when_false);
        let _ = r#else(self);
        self.builder.goto(join, span);
        self.builder.switch_to(join);
        if self.builder.is_reachable() {
            Ok(())
        } else {
            Err(Diverged)
        }
    }

    /// Evaluates `condition` and switches on it. Returns the block where it
    /// is true and the block where it is false, each of which first ends
    /// the condition's temporaries.
    fn branch(&mut self, condition: ExprId) -> Lowered<(BlockId, BlockId)> {
        let span = self.span(condition);
        let mark = self.builder.temp_mark();
        let value = self.operand(condition);
        let temps = self.builder.take_temps(mark);
        let value = value?;
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
        Ok((when_true, when_false))
    }

    /// The condition of a loop, lowered in the current block `head`.
    /// Returns where the body starts and, unless only `break` leaves the
    /// loop, the block where the condition is false.
    fn loop_condition(
        &mut self,
        condition: Option<ExprId>,
        head: BlockId,
    ) -> Lowered<(BlockId, Option<BlockId>)> {
        match condition {
            Some(condition) if !is_literal_bool(self.hir, condition, true) => {
                let (when_true, when_false) = self.branch(condition)?;
                Ok((when_true, Some(when_false)))
            }
            _ => Ok((head, None)),
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
