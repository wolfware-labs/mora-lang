//! Every body reachable from the entry point, with its symbol.

use std::collections::HashMap;

use mora_hir::{BodyOwner, EntryPoint, ItemId, def_map, instance_fields};
use mora_mir::{Body, Callee, TerminatorKind, mir};
use mora_span::{Db, Project, SourceFile};

use crate::error::CodegenError;
use crate::mangle::mangle;

/// A body: the owner it belongs to, and its index in the owner's `Mir::bodies`
/// (0 is the owner's own body).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BodyKey {
    /// The project of the file.
    pub project: Project,
    /// The file that holds the owner.
    pub file: SourceFile,
    /// What the body belongs to.
    pub owner: BodyOwner,
    /// The index in `Mir::bodies`.
    pub local: usize,
}

/// The position of a function in [`Program::functions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

/// One body to generate code for.
pub struct Function<'db> {
    /// Where the body comes from.
    pub key: BodyKey,
    /// The body.
    pub body: &'db Body,
    /// Its symbol.
    pub symbol: String,
}

/// Every body reachable from the entry point.
pub struct Program<'db> {
    db: &'db dyn Db,
    functions: Vec<Function<'db>>,
    index: HashMap<BodyKey, FunctionId>,
    entry: FunctionId,
}

/// The name of the body of the top-level statements.
const TOP_LEVEL: &str = "top-level";

impl<'db> Program<'db> {
    /// Collects the bodies reachable from the entry point of `project`.
    ///
    /// # Errors
    ///
    /// [`CodegenError::NoEntryPoint`] when the project has none.
    pub fn collect(db: &'db dyn Db, project: Project) -> Result<Self, CodegenError> {
        let entry = match def_map(db, project).entry_point() {
            EntryPoint::Statements(file) => BodyKey {
                project,
                file,
                owner: BodyOwner::TopLevel,
                local: 0,
            },
            EntryPoint::Main(item) => item_key(item),
            EntryPoint::None | EntryPoint::Conflict => return Err(CodegenError::NoEntryPoint),
        };
        let root = project.root_namespace(db);
        let mut program = Self {
            db,
            functions: Vec::new(),
            index: HashMap::new(),
            entry: FunctionId(0),
        };
        let mut pending = vec![entry];
        program.add(entry, root);
        while let Some(key) = pending.pop() {
            let caller = program.functions[program.index[&key].0 as usize].body;
            for block in &caller.blocks {
                let TerminatorKind::Call { callee, .. } = &block.terminator.kind else {
                    continue;
                };
                let Some(target) = target_key(db, key, callee) else {
                    continue;
                };
                if !program.index.contains_key(&target) {
                    program.add(target, root);
                    pending.push(target);
                }
            }
        }
        Ok(program)
    }

    /// A program of the single body `body`, which is also its entry.
    #[cfg(test)]
    pub(crate) fn from_body(
        db: &'db dyn Db,
        project: Project,
        file: SourceFile,
        body: &'db Body,
    ) -> Self {
        let key = BodyKey {
            project,
            file,
            owner: BodyOwner::TopLevel,
            local: 0,
        };
        let mut index = HashMap::new();
        index.insert(key, FunctionId(0));
        Self {
            db,
            functions: vec![Function {
                key,
                body,
                symbol: symbol_of(&body.name, project.root_namespace(db)),
            }],
            index,
            entry: FunctionId(0),
        }
    }

    fn add(&mut self, key: BodyKey, root: &str) {
        let owner_mir = mir(self.db, key.project, key.file, key.owner);
        let Some(body) = owner_mir.bodies.get(key.local) else {
            panic!("codegen: `{:?}` has no body {}", key.owner, key.local);
        };
        let id = function_id(self.functions.len());
        self.index.insert(key, id);
        self.functions.push(Function {
            key,
            body,
            symbol: symbol_of(&body.name, root),
        });
    }

    /// The database the program was collected from.
    pub fn db(&self) -> &'db dyn Db {
        self.db
    }

    /// Every function; the entry first.
    pub fn functions(&self) -> &[Function<'db>] {
        &self.functions
    }

    /// The entry point.
    pub fn entry(&self) -> FunctionId {
        self.entry
    }

    /// The function `caller` reaches through `callee`; `None` for a function
    /// of the standard library.
    pub fn callee(&self, caller: FunctionId, callee: &Callee) -> Option<FunctionId> {
        let from = self.functions.get(caller.0 as usize)?.key;
        let target = target_key(self.db, from, callee)?;
        self.index.get(&target).copied()
    }
}

/// The id of the function at position `position`.
///
/// # Panics
///
/// When there are more functions than ids.
fn function_id(position: usize) -> FunctionId {
    let Ok(id) = u32::try_from(position) else {
        panic!("codegen: a program cannot have {position} functions");
    };
    FunctionId(id)
}

fn item_key(item: ItemId) -> BodyKey {
    BodyKey {
        project: item.project,
        file: item.file,
        owner: BodyOwner::Item(item.item),
        local: 0,
    }
}

/// The body `callee` names when called from `from`.
fn target_key(db: &dyn Db, from: BodyKey, callee: &Callee) -> Option<BodyKey> {
    match callee {
        Callee::Item(item) => Some(item_key(*item)),
        Callee::Local(local) => Some(BodyKey {
            local: *local,
            ..from
        }),
        Callee::Initializer(item, n) => {
            let (index, _) = instance_fields(db, *item).nth(*n as usize)?;
            Some(BodyKey {
                project: item.project,
                file: item.file,
                owner: BodyOwner::Value(item.item, index),
                local: 0,
            })
        }
        Callee::External(_) => None,
    }
}

/// The symbol of a body named `name`. A body in the top-level statements is
/// named below `top-level`, which stands for the root namespace.
fn symbol_of(name: &str, root: &str) -> String {
    let mut segments = name.split('.').peekable();
    if segments.peek() == Some(&TOP_LEVEL) {
        mangle(root.split('.').chain(segments))
    } else {
        mangle(segments)
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::TestDb;
    use mora_mir::Callee;
    use mora_span::ProjectKind;

    use super::*;
    use crate::test_util::{app_of, symbol};

    fn symbols(program: &Program<'_>) -> Vec<String> {
        program
            .functions()
            .iter()
            .map(|f| f.symbol.clone())
            .collect()
    }

    #[test]
    fn collect_starts_at_the_top_level_statements() {
        let db = TestDb::default();
        let project = app_of(&db, "F(): int => 1;\nF();\n");
        let program = Program::collect(&db, project).unwrap();
        let entry = &program.functions()[program.entry().0 as usize];
        assert_eq!(entry.symbol, mangle(["UsersApi", "top-level"]));
        assert!(symbols(&program).contains(&symbol("F")));
    }

    #[test]
    fn collect_starts_at_main() {
        let db = TestDb::default();
        let project = app_of(&db, "Main(): int { return 7; }\n");
        let program = Program::collect(&db, project).unwrap();
        assert_eq!(program.functions().len(), 1);
        assert_eq!(program.functions()[0].symbol, symbol("Main"));
        assert_eq!(program.entry(), FunctionId(0));
    }

    #[test]
    fn a_library_has_no_entry_point() {
        let db = TestDb::default();
        let project = db.project(
            "lib",
            ProjectKind::Lib,
            &[("src/Lib.mr", "F(): int => 1;\n")],
            Vec::new(),
        );
        assert!(matches!(
            Program::collect(&db, project),
            Err(CodegenError::NoEntryPoint)
        ));
    }

    #[test]
    fn collect_skips_bodies_nothing_calls() {
        let db = TestDb::default();
        let project = app_of(&db, "Unused() { let a = new int[3]; }\nlet x = 1;\n");
        let program = Program::collect(&db, project).unwrap();
        assert_eq!(program.functions().len(), 1);
    }

    #[test]
    fn collect_follows_local_functions_and_field_initializers() {
        let db = TestDb::default();
        let project = app_of(
            &db,
            "struct P { public X: int = 4; public Y: int; }\nOuter(y: int): int { Inner(): int => 3; let p = new P { Y = y }; return Inner() + p.X; }\nOuter(1);\n",
        );
        let program = Program::collect(&db, project).unwrap();
        let symbols = symbols(&program);
        assert!(symbols.contains(&symbol("Outer")), "{symbols:?}");
        assert!(
            symbols.contains(&mangle(["UsersApi", "Outer", "Inner"])),
            "{symbols:?}"
        );
        assert!(
            symbols.contains(&mangle(["UsersApi", "P", "X"])),
            "{symbols:?}"
        );
    }

    #[test]
    fn a_local_function_of_the_top_level_statements_has_the_root_prefix() {
        let db = TestDb::default();
        let project = app_of(&db, "if true { Helper(): int => 2; let x = Helper(); }\n");
        let program = Program::collect(&db, project).unwrap();
        let symbols = symbols(&program);
        assert!(
            symbols.contains(&mangle(["UsersApi", "top-level", "Helper"])),
            "{symbols:?}"
        );
    }

    #[test]
    fn callee_maps_calls_to_collected_functions() {
        let db = TestDb::default();
        let project = app_of(&db, "F(): int => 1;\nF();\n");
        let program = Program::collect(&db, project).unwrap();
        let top = program.entry();
        assert_eq!(
            program.callee(top, &Callee::External(vec!["Std".into()])),
            None
        );
        let calls: Vec<&Callee> = program.functions()[0]
            .body
            .blocks
            .iter()
            .filter_map(|b| match &b.terminator.kind {
                TerminatorKind::Call { callee, .. } => Some(callee),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1);
        let target = program.callee(top, calls[0]).unwrap();
        assert_eq!(program.functions()[target.0 as usize].symbol, symbol("F"));
    }

    #[test]
    fn from_body_makes_a_one_function_program() {
        let db = TestDb::default();
        let project = app_of(&db, "Main(): int { return 7; }\n");
        let full = Program::collect(&db, project).unwrap();
        let f = &full.functions()[0];
        let single = Program::from_body(&db, f.key.project, f.key.file, f.body);
        assert_eq!(single.functions()[0].symbol, symbol("Main"));
        assert_eq!(single.entry(), FunctionId(0));
    }

    #[test]
    fn callee_resolves_local_functions_and_initializers() {
        let db = TestDb::default();
        let project = app_of(
            &db,
            "struct P { public X: int = 4; public Y: int; }\nOuter(y: int): int { Inner(): int => 3; let p = new P { Y = y }; return Inner() + p.X; }\nOuter(1);\n",
        );
        let program = Program::collect(&db, project).unwrap();
        let outer = program
            .functions()
            .iter()
            .position(|f| f.symbol == symbol("Outer"))
            .unwrap();
        let outer_id = FunctionId(u32::try_from(outer).unwrap());
        let mut resolved = Vec::new();
        for block in &program.functions()[outer].body.blocks {
            if let TerminatorKind::Call { callee, .. } = &block.terminator.kind {
                let id = program.callee(outer_id, callee).unwrap();
                resolved.push(program.functions()[id.0 as usize].symbol.clone());
            }
        }
        assert!(
            resolved.contains(&mangle(["UsersApi", "Outer", "Inner"])),
            "{resolved:?}"
        );
        assert!(
            resolved.contains(&mangle(["UsersApi", "P", "X"])),
            "{resolved:?}"
        );
        assert_eq!(program.callee(FunctionId(999), &Callee::Local(0)), None);
        assert_eq!(program.callee(outer_id, &Callee::Local(0)), Some(outer_id));
    }

    #[test]
    fn symbols_prefix_a_dotted_root_namespace() {
        assert_eq!(
            symbol_of("top-level.Helper", "Acme.Users"),
            mangle(["Acme", "Users", "top-level", "Helper"])
        );
        assert_eq!(
            symbol_of("top-level", "Acme.Users"),
            mangle(["Acme", "Users", "top-level"])
        );
        assert_eq!(symbol_of("A.B.C", "Acme.Users"), mangle(["A", "B", "C"]));
    }

    #[test]
    #[should_panic(expected = "has no body 99")]
    fn a_missing_local_body_is_an_internal_error() {
        let db = TestDb::default();
        let project = app_of(&db, "Main(): int { return 7; }\n");
        let mut program = Program::collect(&db, project).unwrap();
        let key = BodyKey {
            local: 99,
            ..program.functions()[0].key
        };
        program.add(key, "UsersApi");
    }

    #[test]
    #[should_panic(expected = "cannot have")]
    fn too_many_functions_is_an_internal_error() {
        function_id(usize::MAX);
    }
}
