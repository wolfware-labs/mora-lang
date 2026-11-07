//! Calls: which function a call calls, and its arguments checked against
//! that function's parameters (§6.9, §10.3).

use std::collections::HashMap;

use super::Check;
use crate::arena::arena_index;
use crate::bodies::{Arg, ArgMode, Expr, ExprId, LocalId, LocalKind, Stmt, Target};
use crate::defs::ItemId;
use crate::inference::constant::{self, Value};
use crate::inference::{
    Callee, Context, NamedProblem, TypeError, converts, is_known, numeric, same,
};
use crate::items::{ItemKind, signature_refs};
use crate::resolve::{Resolved, item_data};
use crate::signature::signature;
use crate::ty::{ParamMode, Primitive, Signature, Ty};
use mora_syntax::lexer::number::too_large_for_float;

/// A function a call may call.
struct Candidate {
    /// What it is.
    origin: Callee,
    /// Its name.
    name: String,
    /// Its parameters, in order.
    params: Vec<Parameter>,
    /// What a call of it gives, `None` when that is unknown, as for an
    /// `async` function.
    ret: Option<Ty>,
    /// Whether its calls are not checked yet: a generic function, whose
    /// type arguments are inferred later (M3.8), or one with a `params`
    /// parameter.
    unchecked: bool,
}

/// A parameter of a [`Candidate`].
struct Parameter {
    /// Its name.
    name: String,
    /// Its mode, `None` when it is not known, as for a local function.
    mode: Option<ParamMode>,
    /// Whether it has a default value.
    has_default: bool,
    /// Its type.
    ty: Ty,
}

/// What a callee is.
enum Callees {
    /// Something whose calls a later milestone checks.
    Unknown,
    /// A value of a known type that is not a function (`MR0208`).
    NotCallable(Ty),
    /// Functions, one of which the call calls.
    Functions(Vec<Candidate>),
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

/// How an argument converts to a parameter's type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conversion {
    /// It has the type, or a literal takes it.
    Identity,
    /// It converts implicitly (§5.10).
    Implicit,
    /// Its type, or the parameter's, is unknown: it may convert.
    Unknown,
    /// It does not convert.
    None,
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

impl Check<'_> {
    /// A call: the function it calls, the arguments checked against its
    /// parameters, and its return type.
    pub(super) fn call(&mut self, expr: ExprId, callee: ExprId, args: &[Arg]) -> Option<Ty> {
        let candidates = match self.callees(callee) {
            Callees::Unknown => return self.unchecked_call(args),
            Callees::NotCallable(ty) => {
                self.errors
                    .push(TypeError::NotCallable { expr: callee, ty });
                self.unchecked_call(args);
                return Some(Ty::Error);
            }
            Callees::Functions(candidates) => candidates,
        };
        if candidates.is_empty() || candidates.iter().any(|candidate| candidate.unchecked) {
            return self.unchecked_call(args);
        }
        if !self.named_arguments_in_order(args) {
            return self.unchecked_call(args);
        }
        let arguments: Vec<Argument> = args
            .iter()
            .map(|arg| match constant::value(&self.body.exprs, arg.expr) {
                Some(value) => Argument::Literal(value),
                None => Argument::Typed(self.infer(arg.expr)),
            })
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
        for arg in args {
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

    /// What the callee of a call is.
    fn callees(&mut self, callee: ExprId) -> Callees {
        let body = self.body;
        match &body.exprs[callee] {
            Expr::Path(path) => {
                let Some(resolution) = self.resolution.paths.get(&callee) else {
                    return Callees::Unknown;
                };
                let named = resolution.segments.max(1);
                if named == path.segments.len() {
                    return match &resolution.target {
                        Target::Item(Resolved::Functions(items)) => self.functions(items),
                        _ => self.not_callable(self.target(&resolution.target)),
                    };
                }
                // The last segment is a method of the value the others name.
                let mut receiver = self.target(&resolution.target);
                let last = path.segments.len() - 1;
                for name in &path.segments[named..last] {
                    receiver = self.field(receiver.as_ref(), name);
                }
                self.methods(receiver.as_ref(), &path.segments[last])
            }
            Expr::Member {
                receiver,
                name,
                optional,
                ..
            } => {
                let receiver = self.infer(*receiver);
                if let Some(resolution) = self.resolution.paths.get(&callee) {
                    return match &resolution.target {
                        Target::Item(Resolved::Functions(items)) => self.functions(items),
                        _ => self.not_callable(self.target(&resolution.target)),
                    };
                }
                // `x?.M()` gives an option (§7.5).
                if *optional {
                    return Callees::Unknown;
                }
                self.methods(receiver.as_ref(), name)
            }
            Expr::Local(local) if body.locals[*local].kind == LocalKind::Function => self
                .local_function(*local)
                .map_or(Callees::Unknown, |candidate| {
                    Callees::Functions(vec![candidate])
                }),
            _ => {
                let ty = self.infer(callee);
                self.not_callable(ty)
            }
        }
    }

    /// `MR0208` for a callee of a known type, which is not a function type,
    /// since those are not known yet.
    fn not_callable(&self, ty: Option<Ty>) -> Callees {
        match ty {
            Some(ty) if is_known(&ty) => Callees::NotCallable(ty),
            _ => Callees::Unknown,
        }
    }

    /// The candidates of a call to `items`, a function or overloads.
    fn functions(&self, items: &[ItemId]) -> Callees {
        Callees::Functions(
            items
                .iter()
                .filter_map(|&item| self.item_candidate(item))
                .collect(),
        )
    }

    /// The non-static methods named `name` of a value of type `receiver`,
    /// when it is a struct that is not generic. A call through a value of
    /// any other type, or of a static method, is not checked yet.
    fn methods(&self, receiver: Option<&Ty>, name: &str) -> Callees {
        let Some(Ty::Struct(ty)) = receiver else {
            return Callees::Unknown;
        };
        let refs = signature_refs(self.db, ty.file);
        if !refs[arena_index(ty.item)].type_params.is_empty() {
            return Callees::Unknown;
        }
        let tree = crate::items::item_tree(self.db, ty.file);
        let methods: Vec<_> = item_data(self.db, *ty)
            .members
            .iter()
            .filter(|&&member| {
                let data = tree.item(member);
                data.kind == ItemKind::Function && data.name == name && !data.is_static
            })
            .filter_map(|&member| {
                self.item_candidate(ItemId {
                    project: ty.project,
                    file: ty.file,
                    item: member,
                })
            })
            .collect();
        if methods.is_empty() {
            let field = self.field(receiver, name);
            return self.not_callable(field);
        }
        Callees::Functions(methods)
    }

    /// A function or method as a candidate.
    fn item_candidate(&self, item: ItemId) -> Option<Candidate> {
        let Signature::Function(function) = signature(self.db, item) else {
            return None;
        };
        let generic = !signature_refs(self.db, item.file)[arena_index(item.item)]
            .type_params
            .is_empty();
        Some(Candidate {
            origin: Callee::Item(item),
            name: item_data(self.db, item).name.clone(),
            params: function
                .params
                .iter()
                .map(|param| Parameter {
                    name: param.name.clone(),
                    mode: Some(param.mode),
                    has_default: param.has_default,
                    ty: param.ty.clone(),
                })
                .collect(),
            ret: (!function.is_async).then(|| function.ret.clone()),
            unchecked: generic || function.params.iter().any(|param| param.is_params),
        })
    }

    /// The local function `local` as a candidate.
    fn local_function(&mut self, local: LocalId) -> Option<Candidate> {
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
        let ret = function.ret.map_or(Ty::UNIT, |ty| self.written(ty));
        Some(Candidate {
            origin: Callee::Local(local),
            name: body.locals[local].name.clone(),
            params: function
                .params
                .iter()
                .map(|&param| Parameter {
                    name: body.locals[param].name.clone(),
                    // Local functions do not record their parameters' modes.
                    mode: None,
                    has_default: function.defaults.iter().any(|&(at, _)| at == param),
                    ty: self.locals[arena_index(param)].clone().unwrap_or(Ty::Error),
                })
                .collect(),
            ret: (!function.is_async).then_some(ret),
            unchecked: generic,
        })
    }

    /// A call of a single declaration: its problems are reported precisely,
    /// and the call has its return type.
    fn call_one(
        &mut self,
        expr: ExprId,
        candidate: &Candidate,
        args: &[Arg],
        arguments: &[Argument],
    ) -> Option<Ty> {
        let signature = self.signature_text(candidate);
        let params = match bind(candidate, args) {
            Ok(params) => params,
            Err(unbound) => {
                self.infer_literals(args, arguments);
                let name = |arg: usize| args[arg].name.clone().unwrap_or_default();
                self.errors.push(match unbound {
                    Unbound::TooMany(first) => TypeError::Arity {
                        expr: args[first].expr,
                        signature,
                        missing: None,
                    },
                    Unbound::Missing(param) => TypeError::Arity {
                        expr,
                        signature,
                        missing: Some(candidate.params[param].name.clone()),
                    },
                    Unbound::UnknownName(arg) => TypeError::NamedArgument {
                        arg: args[arg].expr,
                        problem: NamedProblem::Unknown {
                            name: name(arg),
                            function: candidate.name.clone(),
                        },
                    },
                    Unbound::AlreadyGiven(arg) => TypeError::NamedArgument {
                        arg: args[arg].expr,
                        problem: NamedProblem::AlreadyGiven { name: name(arg) },
                    },
                });
                return candidate.ret.clone();
            }
        };
        for ((arg, argument), &param) in args.iter().zip(arguments).zip(&params) {
            let parameter = &candidate.params[param];
            if !mode_fits(parameter.mode, arg.mode) {
                self.errors.push(TypeError::Mode {
                    arg: arg.expr,
                    signature: signature.clone(),
                    param: parameter.name.clone(),
                    needs: parameter.mode.unwrap_or(ParamMode::In),
                    given: arg.mode,
                });
            }
            self.argument(arg.expr, argument, &parameter.ty);
        }
        self.calls.insert(expr, candidate.origin);
        candidate.ret.clone()
    }

    /// A call of overloads: the best applicable one is called (§6.9).
    fn call_overloads(
        &mut self,
        expr: ExprId,
        candidates: &[Candidate],
        args: &[Arg],
        arguments: &[Argument],
    ) -> Option<Ty> {
        // Each applicable candidate, with the parameter of each argument and
        // how each argument converts to it.
        let applicable: Vec<(usize, Vec<usize>, Vec<Conversion>)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let params = bind(candidate, args).ok()?;
                let conversions: Vec<_> = args
                    .iter()
                    .zip(arguments)
                    .zip(&params)
                    .map(|((arg, argument), &param)| {
                        let parameter = &candidate.params[param];
                        if mode_fits(parameter.mode, arg.mode) {
                            conversion(argument, &parameter.ty)
                        } else {
                            Conversion::None
                        }
                    })
                    .collect();
                (!conversions.contains(&Conversion::None)).then_some((index, params, conversions))
            })
            .collect();
        let listed = |check: &Self, indexes: &mut dyn Iterator<Item = usize>| {
            indexes
                .map(|index| {
                    let candidate = &candidates[index];
                    (candidate.origin, check.signature_text(candidate))
                })
                .collect::<Vec<_>>()
        };
        if applicable.is_empty() {
            self.infer_literals(args, arguments);
            let all = listed(self, &mut (0..candidates.len()));
            self.errors.push(TypeError::NoOverload {
                expr,
                name: candidates[0].name.clone(),
                candidates: all,
            });
            return None;
        }
        let beats = |a: &(usize, Vec<usize>, Vec<Conversion>),
                     b: &(usize, Vec<usize>, Vec<Conversion>)| {
            better(&candidates[a.0], a, &candidates[b.0], b)
        };
        let best = applicable
            .iter()
            .find(|a| applicable.iter().all(|b| a.0 == b.0 || beats(a, b)));
        let Some((index, params, _)) = best else {
            self.infer_literals(args, arguments);
            // An argument of unknown type may be what makes the call
            // ambiguous, so it is not reported.
            let unknown = arguments
                .iter()
                .any(|argument| matches!(argument, Argument::Typed(ty) if !ty.as_ref().is_some_and(is_known)));
            if !unknown {
                let tied = listed(self, &mut applicable.iter().map(|(index, ..)| *index));
                self.errors.push(TypeError::Ambiguous {
                    expr,
                    name: candidates[0].name.clone(),
                    candidates: tied,
                });
            }
            return None;
        };
        let candidate = &candidates[*index];
        for ((arg, argument), &param) in args.iter().zip(arguments).zip(params) {
            self.argument(arg.expr, argument, &candidate.params[param].ty);
        }
        self.calls.insert(expr, candidate.origin);
        candidate.ret.clone()
    }

    /// Checks an argument against its parameter's type: a literal takes the
    /// type now, and any other argument was inferred already.
    fn argument(&mut self, expr: ExprId, argument: &Argument, ty: &Ty) {
        match argument {
            Argument::Literal(_) => self.check(expr, ty),
            Argument::Typed(found) => self.coerce(expr, found.as_ref(), ty, Context::Value),
        }
    }

    /// A candidate as code declares it, as `Add(a: int, b: int = ...): int`.
    fn signature_text(&self, candidate: &Candidate) -> String {
        let params: Vec<_> = candidate
            .params
            .iter()
            .map(|param| {
                let mode = match param.mode {
                    Some(ParamMode::Ref) => "ref ",
                    Some(ParamMode::Move) => "move ",
                    _ => "",
                };
                let default = if param.has_default { " = ..." } else { "" };
                let ty = crate::inference::ty_name(self.db, &param.ty);
                format!("{mode}{}: {ty}{default}", param.name)
            })
            .collect();
        let ret = match &candidate.ret {
            Some(ret) if *ret != Ty::UNIT => {
                format!(": {}", crate::inference::ty_name(self.db, ret))
            }
            _ => String::new(),
        };
        format!("{}({}){ret}", candidate.name, params.join(", "))
    }
}

/// Which parameter of `candidate` each argument goes to: by position, then
/// by name, with omitted parameters taking their defaults (§6.9).
fn bind(candidate: &Candidate, args: &[Arg]) -> Result<Vec<usize>, Unbound> {
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
/// `move` only for a parameter of that mode. An unknown mode suits any.
fn mode_fits(needs: Option<ParamMode>, given: Option<ArgMode>) -> bool {
    match (needs, given) {
        (None, _) => true,
        (Some(ParamMode::Ref), given) => given == Some(ArgMode::Ref),
        (Some(_), None) => true,
        (Some(ParamMode::In), Some(given)) => given == ArgMode::In,
        (Some(ParamMode::Move), Some(given)) => given == ArgMode::Move,
    }
}

/// How an argument converts to a parameter of type `ty`.
fn conversion(argument: &Argument, ty: &Ty) -> Conversion {
    if !is_known(ty) {
        return Conversion::Unknown;
    }
    match argument {
        Argument::Typed(None) => Conversion::Unknown,
        Argument::Typed(Some(found)) if !is_known(found) => Conversion::Unknown,
        Argument::Typed(Some(found)) if same(found, ty) => Conversion::Identity,
        Argument::Typed(Some(found)) if converts(found, ty) => Conversion::Implicit,
        Argument::Typed(Some(_)) => Conversion::None,
        Argument::Literal(value) => literal_conversion(*value, ty),
    }
}

/// How a literal, or an operation made only of literals, converts to a
/// parameter of the known type `ty`: it takes a numeric type that holds its
/// value, as for any target (§3.5), and otherwise converts as the type it
/// would have without one.
fn literal_conversion(value: Value, ty: &Ty) -> Conversion {
    let Some(target) = numeric::as_numeric(ty) else {
        return Conversion::None;
    };
    let takes = match value {
        Value::Int(value) => numeric::holds(target, value),
        Value::Real(value) => match target {
            Primitive::Float => !too_large_for_float(value),
            Primitive::Double => true,
            _ => false,
        },
        // Too large to compute: MR0010 says so once the call is chosen.
        Value::Overflow | Value::Infinity | Value::OverflowReal => true,
    };
    if takes {
        return Conversion::Identity;
    }
    let natural = match value {
        Value::Int(value) => numeric::default_integer(value),
        _ => Some(Primitive::Double),
    };
    match natural {
        Some(natural) if numeric::converts(natural, target) => Conversion::Implicit,
        _ => Conversion::None,
    }
}

/// Whether the applicable candidate `a` is better than `b` (§6.9): no
/// argument converts worse to it and one converts better, or, when neither
/// is better that way, it needs no default and the other does.
fn better(
    a: &Candidate,
    (_, a_params, a_conversions): &(usize, Vec<usize>, Vec<Conversion>),
    b: &Candidate,
    (_, b_params, b_conversions): &(usize, Vec<usize>, Vec<Conversion>),
) -> bool {
    let mut some_better = false;
    for index in 0..a_params.len() {
        let pa = &a.params[a_params[index]].ty;
        let pb = &b.params[b_params[index]].ty;
        match compare(a_conversions[index], pa, b_conversions[index], pb) {
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Greater => some_better = true,
            std::cmp::Ordering::Equal => {}
        }
    }
    if some_better {
        return true;
    }
    let defaults = |candidate: &Candidate, params: &[usize]| candidate.params.len() > params.len();
    !defaults(a, a_params) && defaults(b, b_params)
}

/// Whether an argument's conversion to `pa` is better (`Greater`), worse or
/// neither than its conversion to `pb` (§6.9).
fn compare(ca: Conversion, pa: &Ty, cb: Conversion, pb: &Ty) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if ca == Conversion::Unknown || cb == Conversion::Unknown {
        return Ordering::Equal;
    }
    match (ca == Conversion::Identity, cb == Conversion::Identity) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    match (converts(pa, pb), converts(pb, pa)) {
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_suit_their_modes() {
        assert!(mode_fits(Some(ParamMode::Ref), Some(ArgMode::Ref)));
        assert!(!mode_fits(Some(ParamMode::Ref), None));
        assert!(!mode_fits(Some(ParamMode::Ref), Some(ArgMode::In)));
        assert!(mode_fits(Some(ParamMode::In), None));
        assert!(mode_fits(Some(ParamMode::In), Some(ArgMode::In)));
        assert!(!mode_fits(Some(ParamMode::In), Some(ArgMode::Ref)));
        assert!(!mode_fits(Some(ParamMode::In), Some(ArgMode::Move)));
        assert!(mode_fits(Some(ParamMode::Move), Some(ArgMode::Move)));
        assert!(mode_fits(Some(ParamMode::Move), None));
        assert!(!mode_fits(Some(ParamMode::Move), Some(ArgMode::In)));
        assert!(mode_fits(None, Some(ArgMode::Ref)));
    }

    #[test]
    fn a_literal_takes_a_type_that_holds_it() {
        let int = Ty::Primitive(Primitive::Int);
        let byte = Ty::Primitive(Primitive::Byte);
        let float = Ty::Primitive(Primitive::Float);
        let long = Ty::Primitive(Primitive::Long);
        assert!(literal_conversion(Value::Int(1), &byte) == Conversion::Identity);
        assert!(literal_conversion(Value::Int(300), &byte) == Conversion::None);
        assert!(literal_conversion(Value::Int(3_000_000_000), &long) == Conversion::Identity);
        assert!(literal_conversion(Value::Real(1.5), &float) == Conversion::Identity);
        assert!(literal_conversion(Value::Real(1.5), &int) == Conversion::None);
        assert!(literal_conversion(Value::Real(1e39), &float) == Conversion::None);
        assert!(literal_conversion(Value::Overflow, &int) == Conversion::Identity);
        assert!(literal_conversion(Value::Int(1), &Ty::String) == Conversion::None);
        // An integer that is not exact in a `float` would be an `int`.
        assert!(literal_conversion(Value::Int(16_777_217), &float) == Conversion::None);
        assert!(
            literal_conversion(Value::Int(16_777_217), &Ty::Primitive(Primitive::Double))
                == Conversion::Identity
        );
    }
}
