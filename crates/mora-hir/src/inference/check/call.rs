//! Calls: which function a call calls, and its arguments checked against
//! that function's parameters (§6.9, §10.3).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;

use super::{Check, Expect};
use crate::arena::arena_index;
use crate::bodies::{Arg, ArgMode, Expr, ExprId, LocalId, LocalKind, Stmt, Target};
use crate::defs::ItemId;
use crate::diverging::calls_diverging;
use crate::inference::constant::{self, Value};
use crate::inference::{
    Callee, Context, NamedProblem, TypeError, converts, is_known, numeric, same, ty_name,
};
use crate::items::{ItemKind, ValueKind, item_tree};
use crate::resolve::{Resolved, is_visible, item_data};
use crate::signature::signature;
use crate::ty::{ParamMode, Signature, Ty};

/// A function a call may call, borrowed from its signature.
struct Candidate<'a> {
    /// What it is.
    origin: Callee,
    /// Its name.
    name: &'a str,
    /// Its parameters, in order.
    params: Vec<Parameter<'a>>,
    /// What a call of it gives.
    ret: Cow<'a, Ty>,
    /// Whether its calls are not checked yet (D-137): a generic function,
    /// whose type arguments are inferred later (M3.8), one with a `params`
    /// parameter, or an `async` one, which takes every argument by `move`
    /// and gives a future (§10.3, §12).
    unchecked: bool,
}

/// A parameter of a [`Candidate`].
struct Parameter<'a> {
    /// Its name.
    name: &'a str,
    /// Its mode.
    mode: ParamMode,
    /// Whether it has a default value.
    has_default: bool,
    /// Its type.
    ty: Cow<'a, Ty>,
}

impl Parameter<'_> {
    /// What expects its argument's type: a `ref` argument has exactly the
    /// parameter's type (§10.3), any other converts to it.
    fn context(&self) -> Context {
        match self.mode {
            ParamMode::Ref => Context::Ref,
            ParamMode::In | ParamMode::Move => Context::Value,
        }
    }
}

/// What a callee is.
enum Callees<'a> {
    /// Something whose calls a later milestone checks.
    Unknown,
    /// A value of a known type that is not a function (`MR0208`).
    NotCallable(Ty),
    /// Methods of the name, none of them visible from the call: the first
    /// (`MR0129`).
    Hidden(ItemId),
    /// Functions, one of which the call calls.
    Functions(Vec<Candidate<'a>>),
}

/// The type of an argument before a function is chosen.
#[derive(Clone)]
enum Argument {
    /// An operation made only of literals, or a literal, which takes its
    /// parameter's type when its value fits (§3.5).
    Literal(Value),
    /// Any other argument, with its type.
    Typed(Option<Ty>),
}

impl Argument {
    /// Whether its type is not known yet, so that it fits any parameter,
    /// and a call it leaves more than one overload for is not resolved
    /// (D-137).
    fn is_unknown(&self) -> bool {
        matches!(self, Argument::Typed(ty) if !ty.as_ref().is_some_and(is_known))
    }

    /// The type it has without a target, which a conversion to is the
    /// identity (§6.9): its own type, or a literal's (`Value::natural`).
    fn natural(&self) -> Option<Cow<'_, Ty>> {
        match self {
            Argument::Typed(ty) => ty.as_ref().map(Cow::Borrowed),
            Argument::Literal(value) => value.natural().map(|ty| Cow::Owned(Ty::Primitive(ty))),
        }
    }
}

/// Why the arguments of a call do not go to a candidate's parameters.
enum Unbound {
    /// More arguments than parameters; the first extra one.
    TooMany(usize),
    /// A named argument with no parameter of its name.
    UnknownName(usize),
    /// A named argument for a parameter given by position too.
    AlreadyGiven(usize),
    /// A parameter without a default and without an argument.
    Missing(usize),
}

impl<'a> Check<'a> {
    /// A call: the function it calls, the arguments checked against its
    /// parameters, and its return type.
    pub(super) fn call(&mut self, expr: ExprId, callee: ExprId, args: &[Arg]) -> Option<Ty> {
        // The order of named arguments is a rule of every call, and so is
        // calling only what can be called.
        let ordered = self.named_arguments_in_order(args);
        let candidates = match self.callees(callee) {
            // A function of the standard library that never returns has the
            // never type (§11.4); nothing else is known of the library.
            Callees::Unknown if calls_diverging(self.body, self.resolution, callee) => {
                self.unchecked_call(args);
                return Some(Ty::Never);
            }
            // A method of a value that never exists, as `Todo().M()`, is
            // never called either.
            Callees::Unknown if self.exprs[arena_index(callee)] == Some(Ty::Never) => {
                self.unchecked_call(args);
                return Some(Ty::Never);
            }
            Callees::Unknown => return self.unchecked_call(args),
            Callees::NotCallable(ty) => {
                self.errors
                    .push(TypeError::NotCallable { expr: callee, ty });
                self.unchecked_call(args);
                return Some(Ty::Error);
            }
            Callees::Hidden(item) => {
                self.errors.push(TypeError::NotVisible {
                    expr: callee,
                    item,
                    from: self.scope,
                });
                self.unchecked_call(args);
                return Some(Ty::Error);
            }
            Callees::Functions(candidates) => candidates,
        };
        if !ordered
            || candidates.is_empty()
            || candidates.iter().any(|candidate| candidate.unchecked)
        {
            return self.unchecked_call(args);
        }
        // The parser reported an argument list it recovered from: its
        // arguments are not checked, and the call keeps the type that all
        // of its candidates return.
        if args.iter().any(|arg| self.body.is_missing(arg.expr)) {
            self.args(args);
            return common_return(candidates.iter());
        }
        let arguments: Vec<Argument> = args
            .iter()
            .enumerate()
            .map(
                |(position, arg)| match constant::value(&self.body.exprs, arg.expr) {
                    Some(value) => Argument::Literal(value),
                    None => {
                        // A lambda takes the function type of the parameter it
                        // is for, when there is only one function to call.
                        let target = match candidates.as_slice() {
                            [only] if matches!(self.body.exprs[arg.expr], Expr::Lambda(_)) => {
                                match &arg.name {
                                    Some(name) => only.params.iter().find(|p| p.name == name),
                                    None => only.params.get(position),
                                }
                            }
                            _ => None,
                        };
                        Argument::Typed(match target {
                            Some(param) => self.expr(arg.expr, Expect::Target(&param.ty)),
                            None => self.infer(arg.expr),
                        })
                    }
                },
            )
            .collect();
        if let [candidate] = candidates.as_slice() {
            return self.call_one(expr, candidate, args, &arguments);
        }
        self.call_overloads(expr, &candidates, args, &arguments)
    }

    /// The arguments of a call this milestone does not check, inferred so
    /// that nothing inside them is missed.
    fn unchecked_call(&mut self, args: &[Arg]) -> Option<Ty> {
        self.args(args);
        None
    }

    /// The literal arguments, which [`Check::call`] leaves to be checked
    /// against their parameters, inferred without one.
    fn infer_literals(&mut self, args: &[Arg], arguments: &[Argument]) {
        for (arg, argument) in args.iter().zip(arguments) {
            if matches!(argument, Argument::Literal(_)) {
                self.infer(arg.expr);
            }
        }
    }

    /// Reports a named argument followed by a positional one, or a name
    /// given twice (`MR0207`), and says whether there is none.
    fn named_arguments_in_order(&mut self, args: &[Arg]) -> bool {
        let mut named = Vec::new();
        let mut ok = true;
        // A missing argument is the parser's to report.
        for arg in args.iter().filter(|arg| !self.body.is_missing(arg.expr)) {
            let problem = match &arg.name {
                None if !named.is_empty() => Some(NamedProblem::AfterNamed),
                None => None,
                Some(name) if named.contains(&name) => {
                    Some(NamedProblem::Duplicate { name: name.clone() })
                }
                Some(name) => {
                    named.push(name);
                    None
                }
            };
            if let Some(problem) = problem {
                self.errors.push(TypeError::NamedArgument {
                    arg: arg.expr,
                    problem,
                });
                ok = false;
            }
        }
        ok
    }

    /// What the callee of a call is. The callee's own type is recorded, as
    /// for any expression.
    fn callees(&mut self, callee: ExprId) -> Callees<'a> {
        let ty = self.infer(callee);
        let body = self.body;
        match &body.exprs[callee] {
            // A path names what it calls; a member of a value is an
            // `Expr::Member`.
            Expr::Path(_) => self
                .resolution
                .paths
                .get(&callee)
                .map_or(Callees::Unknown, |resolution| {
                    self.resolved(&resolution.target)
                }),
            Expr::Member {
                receiver,
                name,
                optional,
                ..
            } => {
                if let Some(resolution) = self.resolution.paths.get(&callee) {
                    return self.resolved(&resolution.target);
                }
                // `x?.M()` gives an option (§7.5).
                if *optional {
                    return Callees::Unknown;
                }
                self.methods(self.exprs[arena_index(*receiver)].as_ref(), name)
            }
            Expr::Local(local) if body.locals[*local].kind == LocalKind::Function => self
                .local_function(*local)
                .map_or(Callees::Unknown, |candidate| {
                    Callees::Functions(vec![candidate])
                }),
            _ => self.not_callable(ty),
        }
    }

    /// What a callee that name resolution resolved to `target` is.
    fn resolved(&self, target: &Target) -> Callees<'a> {
        match target {
            Target::Item(Resolved::Functions(items)) => self.functions(items),
            _ => self.not_callable(self.target(target)),
        }
    }

    /// `MR0208` for a callee of a known type, which is not a function type,
    /// since those are not known yet.
    fn not_callable(&self, ty: Option<Ty>) -> Callees<'a> {
        match ty {
            Some(ty) if is_known(&ty) => Callees::NotCallable(ty),
            _ => Callees::Unknown,
        }
    }

    /// The candidates of a call to `items`, a function or overloads.
    fn functions(&self, items: &[ItemId]) -> Callees<'a> {
        Callees::Functions(
            items
                .iter()
                .filter_map(|&item| self.item_candidate(item))
                .collect(),
        )
    }

    /// The non-static methods named `name` of a value of type `receiver`,
    /// when it is a struct; a generic struct's type is not `Ty::Struct`. A
    /// call through a value of any other type, or of a static method, is
    /// not checked yet (D-137).
    fn methods(&self, receiver: Option<&Ty>, name: &str) -> Callees<'a> {
        let Some(Ty::Struct(ty)) = receiver else {
            return Callees::Unknown;
        };
        let tree = item_tree(self.db, ty.file);
        let named: Vec<_> = item_data(self.db, *ty)
            .members
            .iter()
            .filter(|&&member| {
                let data = tree.item(member);
                data.kind == ItemKind::Function && data.name == name && !data.is_static
            })
            .map(|&member| ItemId {
                project: ty.project,
                file: ty.file,
                item: member,
            })
            .collect();
        let Some(&first) = named.first() else {
            let field = self.field(receiver, name);
            return self.not_callable(field);
        };
        // Only the methods visible from the call are candidates, as name
        // resolution sees only visible declarations (§4.5).
        let methods: Vec<_> = named
            .into_iter()
            .filter(|&method| is_visible(self.db, method, self.scope))
            .filter_map(|method| self.item_candidate(method))
            .collect();
        if methods.is_empty() {
            return Callees::Hidden(first);
        }
        Callees::Functions(methods)
    }

    /// A function or method as a candidate.
    fn item_candidate(&self, item: ItemId) -> Option<Candidate<'a>> {
        let db = self.db;
        let Signature::Function(function) = signature(db, item) else {
            return None;
        };
        Some(Candidate {
            origin: Callee::Item(item),
            name: &item_data(db, item).name,
            params: function
                .params
                .iter()
                .map(|param| Parameter {
                    name: &param.name,
                    mode: param.mode,
                    has_default: param.has_default,
                    ty: Cow::Borrowed(&param.ty),
                })
                .collect(),
            ret: Cow::Borrowed(&function.ret),
            unchecked: function.is_generic
                || function.is_async
                || function.params.iter().any(|param| param.is_params),
        })
    }

    /// The local function `local` as a candidate.
    fn local_function(&mut self, local: LocalId) -> Option<Candidate<'a>> {
        let body = self.body;
        let functions = self.local_functions.get_or_insert_with(|| {
            body.stmts
                .iter()
                .filter_map(|(id, stmt)| match stmt {
                    Stmt::Function(function) => Some((function.local, id)),
                    _ => None,
                })
                .collect::<HashMap<_, _>>()
        });
        let Stmt::Function(function) = &body.stmts[*functions.get(&local)?] else {
            return None;
        };
        // A local function inside a generic one, or generic itself, has
        // type parameters among its types.
        let generic = function
            .params
            .iter()
            .filter_map(|&param| body.locals[param].ty)
            .chain(function.ret)
            .any(|ty| body.type_params.contains_key(&ty));
        let ret = self.resolution.return_type(function.ret);
        Some(Candidate {
            origin: Callee::Local(local),
            name: &body.locals[local].name,
            params: function
                .params
                .iter()
                .zip(&function.modes)
                .map(|(&param, &mode)| Parameter {
                    name: &body.locals[param].name,
                    mode,
                    has_default: function.defaults.iter().any(|&(at, _)| at == param),
                    ty: Cow::Owned(self.locals[arena_index(param)].clone().unwrap_or(Ty::Error)),
                })
                .collect(),
            ret: Cow::Owned(ret),
            unchecked: generic || function.is_async,
        })
    }

    /// A call of a single declaration: its problems are reported precisely,
    /// and the call has its return type.
    fn call_one(
        &mut self,
        expr: ExprId,
        candidate: &Candidate<'_>,
        args: &[Arg],
        arguments: &[Argument],
    ) -> Option<Ty> {
        let params = match bind(candidate, args) {
            Ok(params) => params,
            Err(unbound) => {
                self.infer_literals(args, arguments);
                self.errors.push(match unbound {
                    Unbound::TooMany(first) => TypeError::Arity {
                        expr: args[first].expr,
                        signature: self.signature_text(candidate),
                        missing: None,
                    },
                    Unbound::Missing(param) => TypeError::Arity {
                        expr,
                        signature: self.signature_text(candidate),
                        missing: Some(candidate.params[param].name.to_owned()),
                    },
                    Unbound::UnknownName(arg) => TypeError::NamedArgument {
                        arg: args[arg].expr,
                        problem: NamedProblem::Unknown {
                            name: args[arg].name.clone().unwrap_or_default(),
                            function: candidate.name.to_owned(),
                        },
                    },
                    Unbound::AlreadyGiven(arg) => already_given(args, arg),
                });
                return Some(Ty::clone(&candidate.ret));
            }
        };
        for ((arg, argument), &param) in args.iter().zip(arguments).zip(&params) {
            let parameter = &candidate.params[param];
            if !mode_fits(parameter.mode, arg.mode) {
                // `ref` makes the call right only before a place of exactly
                // the parameter's type (§10.3), which must be known.
                let exact = matches!(argument, Argument::Typed(Some(found))
                    if is_known(found) && is_known(&parameter.ty) && same(found, &parameter.ty));
                let fixable = exact && self.is_place(arg.expr);
                self.errors.push(TypeError::Mode {
                    arg: arg.expr,
                    signature: self.signature_text(candidate),
                    param: parameter.name.to_owned(),
                    needs: parameter.mode,
                    given: arg.mode,
                    fixable,
                });
            }
            self.argument(arg.expr, argument, parameter);
        }
        self.record_call(expr, candidate.origin, params);
        Some(Ty::clone(&candidate.ret))
    }

    /// A call of overloads: the best applicable one is called (§6.9).
    fn call_overloads(
        &mut self,
        expr: ExprId,
        candidates: &[Candidate<'_>],
        args: &[Arg],
        arguments: &[Argument],
    ) -> Option<Ty> {
        let bindings: Vec<_> = candidates
            .iter()
            .map(|candidate| bind(candidate, args))
            .collect();
        // Each applicable candidate, with the parameter of each argument.
        let applicable: Vec<(usize, &[usize])> = candidates
            .iter()
            .zip(&bindings)
            .enumerate()
            .filter_map(|(index, (candidate, binding))| {
                let params = binding.as_ref().ok()?;
                args.iter()
                    .zip(arguments)
                    .zip(params)
                    .all(|((arg, argument), &param)| {
                        let parameter = &candidate.params[param];
                        mode_fits(parameter.mode, arg.mode) && fits(argument, parameter)
                    })
                    .then_some((index, params.as_slice()))
            })
            .collect();
        // Overloads are declarations, never local functions.
        let listed = |check: &Self, indexes: &mut dyn Iterator<Item = usize>| {
            indexes
                .filter_map(|index| {
                    let candidate = &candidates[index];
                    Some((candidate.origin.item()?, check.signature_text(candidate)))
                })
                .collect::<Vec<_>>()
        };
        if applicable.is_empty() {
            self.infer_literals(args, arguments);
            // A name that every overload's parameters take by position too
            // is the one problem of the call (D-136).
            let given = match bindings.first() {
                Some(Err(Unbound::AlreadyGiven(arg))) => Some(*arg),
                _ => None,
            }
            .filter(|&arg| {
                bindings.iter().all(
                    |binding| matches!(binding, Err(Unbound::AlreadyGiven(other)) if *other == arg),
                )
            });
            self.errors.push(match given {
                Some(arg) => already_given(args, arg),
                None => TypeError::NoOverload {
                    expr,
                    name: candidates[0].name.to_owned(),
                    candidates: listed(self, &mut (0..candidates.len())),
                },
            });
            return None;
        }
        // An argument or a parameter of a type not known yet may be what
        // decides between the overloads, so they are not ranked; the call
        // keeps a type only when they all return it (D-137).
        let unknown_parameter = applicable.iter().any(|&(index, params)| {
            params
                .iter()
                .any(|&param| !is_known(&candidates[index].params[param].ty))
        });
        if applicable.len() > 1 && (unknown_parameter || arguments.iter().any(Argument::is_unknown))
        {
            self.infer_literals(args, arguments);
            return common_return(applicable.iter().map(|&(index, _)| &candidates[index]));
        }
        let beats = |&(a, a_params): &(usize, &[usize]), &(b, b_params): &(usize, &[usize])| {
            better(
                (&candidates[a], a_params),
                (&candidates[b], b_params),
                arguments,
            )
        };
        let best = applicable
            .iter()
            .find(|a| applicable.iter().all(|b| a.0 == b.0 || beats(a, b)));
        let Some(&(index, params)) = best else {
            self.infer_literals(args, arguments);
            let tied = listed(self, &mut applicable.iter().map(|&(index, _)| index));
            self.errors.push(TypeError::Ambiguous {
                expr,
                name: candidates[0].name.to_owned(),
                candidates: tied,
            });
            return None;
        };
        let candidate = &candidates[index];
        for ((arg, argument), &param) in args.iter().zip(arguments).zip(params) {
            self.argument(arg.expr, argument, &candidate.params[param]);
        }
        self.record_call(expr, candidate.origin, params.to_vec());
        Some(Ty::clone(&candidate.ret))
    }

    /// Records that the call `expr` calls `callee`, each argument going to
    /// the parameter at its position in `params`.
    fn record_call(&mut self, expr: ExprId, callee: Callee, params: Vec<usize>) {
        self.calls.insert(expr, callee);
        self.arguments.insert(expr, params);
    }

    /// Checks an argument against its parameter's type: a literal takes the
    /// type now, and any other argument was inferred already. A `ref`
    /// argument has exactly the parameter's type (§10.3).
    fn argument(&mut self, expr: ExprId, argument: &Argument, parameter: &Parameter<'_>) {
        match argument {
            Argument::Literal(_) => self.check(expr, &parameter.ty),
            Argument::Typed(found) => {
                self.coerce(expr, found.as_ref(), &parameter.ty, parameter.context());
            }
        }
    }

    /// Whether `expr` is a place `ref` can pass: a local, a field, or an
    /// element. A path that names a constant or a function is not one.
    fn is_place(&self, expr: ExprId) -> bool {
        match &self.body.exprs[expr] {
            Expr::Local(_)
            | Expr::Member {
                optional: false, ..
            }
            | Expr::Index {
                optional: false, ..
            } => true,
            Expr::Path(_) => self.resolution.paths.get(&expr).is_some_and(|resolution| {
                matches!(&resolution.target, Target::Value(value)
                    if value.member(self.db).is_some_and(|member| member.kind == ValueKind::Field))
            }),
            _ => false,
        }
    }

    /// A candidate as code declares it, as `Add(a: int, b: int = ...): int`.
    fn signature_text(&self, candidate: &Candidate<'_>) -> String {
        let params: Vec<_> = candidate
            .params
            .iter()
            .map(|param| {
                let mode = match param.mode {
                    ParamMode::Ref => "ref ",
                    ParamMode::Move => "move ",
                    ParamMode::In => "",
                };
                let default = if param.has_default { " = ..." } else { "" };
                let ty = ty_name(self.db, &param.ty);
                format!("{mode}{}: {ty}{default}", param.name)
            })
            .collect();
        let ret = if *candidate.ret == Ty::UNIT {
            String::new()
        } else {
            format!(": {}", ty_name(self.db, &candidate.ret))
        };
        format!("{}({}){ret}", candidate.name, params.join(", "))
    }
}

/// The type that every one of `candidates` returns, `None` when they do
/// not agree, the names of tuple elements aside.
fn common_return<'c, 'a: 'c>(
    mut candidates: impl Iterator<Item = &'c Candidate<'a>>,
) -> Option<Ty> {
    let first = candidates.next()?;
    candidates
        .all(|candidate| same(&candidate.ret, &first.ret))
        .then(|| Ty::clone(&first.ret))
}

/// `MR0207` for the named argument `arg`, whose parameter a positional
/// argument already gives.
fn already_given(args: &[Arg], arg: usize) -> TypeError {
    TypeError::NamedArgument {
        arg: args[arg].expr,
        problem: NamedProblem::AlreadyGiven {
            name: args[arg].name.clone().unwrap_or_default(),
        },
    }
}

/// Which parameter of `candidate` each argument goes to: by position, then
/// by name, with omitted parameters taking their defaults (§6.9).
fn bind(candidate: &Candidate<'_>, args: &[Arg]) -> Result<Vec<usize>, Unbound> {
    let mut given = vec![false; candidate.params.len()];
    let mut params = Vec::with_capacity(args.len());
    for (index, arg) in args.iter().enumerate() {
        let param = match &arg.name {
            None if index >= candidate.params.len() => return Err(Unbound::TooMany(index)),
            None => index,
            Some(name) => match candidate
                .params
                .iter()
                .position(|param| param.name == *name)
            {
                Some(param) if given[param] => return Err(Unbound::AlreadyGiven(index)),
                Some(param) => param,
                None => return Err(Unbound::UnknownName(index)),
            },
        };
        given[param] = true;
        params.push(param);
    }
    match candidate
        .params
        .iter()
        .zip(&given)
        .position(|(param, &given)| !given && !param.has_default)
    {
        Some(missing) => Err(Unbound::Missing(missing)),
        None => Ok(params),
    }
}

/// Whether an argument written with `given` before it suits a parameter of
/// mode `needs` (§10.3): `ref` exactly for a `ref` parameter, and `in` or
/// `move` only for a parameter of that mode.
fn mode_fits(needs: ParamMode, given: Option<ArgMode>) -> bool {
    match (needs, given) {
        (ParamMode::Ref, given) => given == Some(ArgMode::Ref),
        (_, None) => true,
        (ParamMode::In, Some(given)) => given == ArgMode::In,
        (ParamMode::Move, Some(given)) => given == ArgMode::Move,
    }
}

/// Whether an argument fits `parameter`: its type is exactly the
/// parameter's for `ref` and converts to it otherwise (`Context::accepts`),
/// it is a literal whose value the type holds (§3.5), or its type or the
/// parameter's is not known yet.
fn fits(argument: &Argument, parameter: &Parameter<'_>) -> bool {
    let ty = &parameter.ty;
    if !is_known(ty) || argument.is_unknown() {
        return true;
    }
    match argument {
        Argument::Typed(found) => found
            .as_ref()
            .is_some_and(|found| parameter.context().accepts(found, ty)),
        Argument::Literal(value) => literal_fits(*value, ty),
    }
}

/// Whether a literal, or an operation made only of literals, takes the
/// known type `ty`: a numeric type that holds its value, as for any target
/// (§3.5). The type it would have without a target converts to no other
/// type that does not hold the value, so this is also where it converts.
fn literal_fits(value: Value, ty: &Ty) -> bool {
    let Some(target) = numeric::as_numeric(ty) else {
        return false;
    };
    match value {
        Value::Int(_) | Value::Real(_) => value.of_type(target).is_some(),
        // Too large to compute: MR0010 says so once the call is chosen.
        Value::Overflow | Value::Infinity | Value::OverflowReal => true,
    }
}

/// Whether the applicable candidate `a` is better than `b` (§6.9): no
/// argument converts worse to it and one converts better, or, when neither
/// is better that way, it needs no default and the other does.
fn better(
    (a, a_params): (&Candidate<'_>, &[usize]),
    (b, b_params): (&Candidate<'_>, &[usize]),
    arguments: &[Argument],
) -> bool {
    let mut some_better = false;
    for (index, argument) in arguments.iter().enumerate() {
        let pa = &a.params[a_params[index]].ty;
        let pb = &b.params[b_params[index]].ty;
        match compare(argument, pa, pb) {
            Ordering::Less => return false,
            Ordering::Greater => some_better = true,
            Ordering::Equal => {}
        }
    }
    if some_better {
        return true;
    }
    let defaults =
        |candidate: &Candidate<'_>, params: &[usize]| candidate.params.len() > params.len();
    !defaults(a, a_params) && defaults(b, b_params)
}

/// Whether an argument's conversion to `pa` is better (`Greater`) than to
/// `pb`, or worse (§6.9). A literal takes any numeric type that holds its
/// value (§3.5), and a conversion of its natural type under §5.10 ranks
/// above taking a type only as a literal. Within a rank, a conversion to
/// `pa` is better when `pa` converts to `pb` and not back. That gives the
/// identity first too: the natural type converts to every type its rank
/// holds, and none of them converts back.
fn compare(argument: &Argument, pa: &Ty, pb: &Ty) -> Ordering {
    let natural = argument.natural();
    let rank = |ty: &Ty| match &natural {
        Some(natural) if converts(natural, ty) => 0,
        _ => 1,
    };
    match rank(pb).cmp(&rank(pa)) {
        Ordering::Equal => match (converts(pa, pb), converts(pb, pa)) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => Ordering::Equal,
        },
        order => order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::Primitive;

    #[test]
    fn markers_suit_their_modes() {
        assert!(mode_fits(ParamMode::Ref, Some(ArgMode::Ref)));
        assert!(!mode_fits(ParamMode::Ref, None));
        assert!(!mode_fits(ParamMode::Ref, Some(ArgMode::In)));
        assert!(mode_fits(ParamMode::In, None));
        assert!(mode_fits(ParamMode::In, Some(ArgMode::In)));
        assert!(!mode_fits(ParamMode::In, Some(ArgMode::Ref)));
        assert!(!mode_fits(ParamMode::In, Some(ArgMode::Move)));
        assert!(mode_fits(ParamMode::Move, Some(ArgMode::Move)));
        assert!(mode_fits(ParamMode::Move, None));
        assert!(!mode_fits(ParamMode::Move, Some(ArgMode::In)));
    }

    #[test]
    fn a_literal_takes_a_type_that_holds_it() {
        let int = Ty::Primitive(Primitive::Int);
        let byte = Ty::Primitive(Primitive::Byte);
        let float = Ty::Primitive(Primitive::Float);
        let double = Ty::Primitive(Primitive::Double);
        let long = Ty::Primitive(Primitive::Long);
        assert!(literal_fits(Value::Int(1), &byte));
        assert!(!literal_fits(Value::Int(300), &byte));
        assert!(literal_fits(Value::Int(3_000_000_000), &long));
        assert!(literal_fits(Value::Real(1.5), &float));
        assert!(literal_fits(Value::Real(1.5), &double));
        assert!(!literal_fits(Value::Real(1.5), &int));
        assert!(!literal_fits(Value::Real(1e39), &float));
        assert!(literal_fits(Value::Overflow, &int));
        assert!(!literal_fits(Value::Int(1), &Ty::String));
        // An integer that is not exact in a `float` would be an `int`.
        assert!(!literal_fits(Value::Int(16_777_217), &float));
        assert!(literal_fits(Value::Int(16_777_217), &double));
    }

    #[test]
    fn only_a_declaration_has_an_item() {
        let local = LocalId::from_raw(la_arena::RawIdx::from_u32(0));
        assert_eq!(Callee::Local(local).item(), None);
    }
}
