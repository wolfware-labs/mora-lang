use std::collections::BTreeMap;

use la_arena::{Arena, Idx};
use mora_span::{Db, Project, ProjectKind, SourceFile};

use crate::items::{ItemIdx, ItemKind, item_tree};
use crate::name::pascal_case;

/// An index into the namespaces of a [`DefMap`].
pub type NamespaceId = Idx<Namespace>;

/// An item of a project: its file and its index in the file's item tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ItemId {
    /// The project the file belongs to.
    pub project: Project,
    /// The file that declares it.
    pub file: SourceFile,
    /// Its index in [`crate::item_tree`] of the file.
    pub item: ItemIdx,
}

/// A namespace of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NamespaceRef {
    /// The project whose [`DefMap`] holds it.
    pub project: Project,
    /// Its index there.
    pub id: NamespaceId,
}

/// The namespaces of a project and what each declares (§4.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefMap {
    /// Every namespace, the root namespace's enclosing ones first.
    namespaces: Arena<Namespace>,
    /// The first segment of the root namespace: the name the project's code
    /// reaches it by.
    outermost: NamespaceId,
    /// The root namespace, where the files directly under `src/` are.
    root: NamespaceId,
    /// Each file and its namespace, by path.
    files: Vec<(SourceFile, NamespaceId)>,
    /// Folders that give no namespace name, or the same one as a sibling.
    folder_problems: Vec<FolderProblem>,
    /// Every `Main` function, by file path and then in the order of the text.
    mains: Vec<ItemId>,
    /// The files with top-level statements, by path.
    statement_files: Vec<SourceFile>,
    /// Whether the project is an app or a library.
    kind: ProjectKind,
    /// The entry file, when the project has it.
    entry_file: Option<SourceFile>,
}

/// One namespace: a folder under `src/`, or a segment of the root namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    /// Its last segment.
    pub name: String,
    /// The namespace it is in; `None` for the first segment of the root namespace.
    pub parent: Option<NamespaceId>,
    /// The namespaces of its subfolders, by name.
    pub children: BTreeMap<String, NamespaceId>,
    /// The items its files declare at namespace level, by name. Several
    /// items with one name are overloads or a conflict.
    pub items: BTreeMap<String, Vec<ItemId>>,
}

/// A folder `MR0121` reports (§4.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderProblem {
    /// The folder's name in PascalCase is not an identifier.
    Invalid {
        /// The folder, such as `src/2d`.
        folder: String,
        /// The first file in it, where the diagnostic points.
        file: SourceFile,
    },
    /// The folder gives the same namespace as a sibling that comes first.
    Clash {
        /// The folder, such as `src/http-client`.
        folder: String,
        /// The sibling, such as `src/HttpClient`.
        other: String,
        /// The namespace both give.
        namespace: String,
        /// The first file in `folder`.
        file: SourceFile,
        /// The first file in `other`.
        other_file: SourceFile,
    },
}

/// What a project starts from (§4.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPoint {
    /// A library, or an app with neither top-level statements nor `Main`.
    None,
    /// The top-level statements of the entry file.
    Statements(SourceFile),
    /// The one `Main` function.
    Main(ItemId),
    /// More than one candidate: `MR0124`.
    Conflict,
}

impl DefMap {
    pub fn namespace(&self, id: NamespaceId) -> &Namespace {
        &self.namespaces[id]
    }

    /// The root namespace, where the files directly under `src/` are.
    pub fn root(&self) -> NamespaceId {
        self.root
    }

    /// The namespace named by the first segment of the root namespace.
    pub fn outermost(&self) -> NamespaceId {
        self.outermost
    }

    /// The namespace of a file of the project.
    pub fn file_namespace(&self, file: SourceFile) -> Option<NamespaceId> {
        self.files
            .iter()
            .find_map(|&(it, namespace)| (it == file).then_some(namespace))
    }

    /// The files of the project, by path.
    pub fn files(&self) -> impl Iterator<Item = SourceFile> + '_ {
        self.files.iter().map(|&(file, _)| file)
    }

    /// The full name of a namespace, such as `UsersApi.Store`.
    pub fn namespace_path(&self, id: NamespaceId) -> String {
        path_of(&self.namespaces, id)
    }

    /// `start` and the namespaces around it, from the innermost out.
    pub fn enclosing_namespaces(
        &self,
        start: Option<NamespaceId>,
    ) -> impl Iterator<Item = NamespaceId> + '_ {
        enclosing(&self.namespaces, start)
    }

    pub fn folder_problems(&self) -> &[FolderProblem] {
        &self.folder_problems
    }

    /// Every `Main` function: one at namespace level, or a `static` one in a
    /// type that is not an interface.
    pub fn mains(&self) -> &[ItemId] {
        &self.mains
    }

    /// The files with top-level statements, by path.
    pub fn statement_files(&self) -> &[SourceFile] {
        &self.statement_files
    }

    pub fn entry_file(&self) -> Option<SourceFile> {
        self.entry_file
    }

    /// The entry point, or why there is not exactly one.
    pub fn entry_point(&self) -> EntryPoint {
        if self.kind == ProjectKind::Lib {
            return EntryPoint::None;
        }
        let statements = self
            .entry_file
            .filter(|file| self.statement_files.contains(file));
        match (statements, self.mains.as_slice()) {
            (None, []) => EntryPoint::None,
            (Some(file), []) => EntryPoint::Statements(file),
            (None, [main]) => EntryPoint::Main(*main),
            _ => EntryPoint::Conflict,
        }
    }
}

/// The def map of a project. It reads the files' paths and item trees,
/// never their text, so an edit that leaves every item tree equal does not
/// run it again.
#[salsa::tracked(returns(ref))]
pub fn def_map(db: &dyn Db, project: Project) -> DefMap {
    let mut namespaces = Arena::new();
    let mut segments = project.root_namespace(db).split('.');
    // `split` yields at least one segment.
    let first = segments.next().unwrap_or_default();
    let outermost = namespaces.alloc(Namespace::new(first.to_owned(), None));
    let mut root = outermost;
    for segment in segments {
        root = add_child(&mut namespaces, root, segment.to_owned());
    }
    let mut builder = Builder {
        namespaces,
        folders: BTreeMap::new(),
        folder_problems: Vec::new(),
    };

    let mut files: Vec<SourceFile> = project.files(db).clone();
    files.sort_by(|a, b| a.path(db).cmp(b.path(db)));
    let mut map_files = Vec::new();
    let mut mains = Vec::new();
    let mut statement_files = Vec::new();
    for file in files {
        let path = file.path(db);
        let namespace = builder.namespace_of(root, path, file);
        map_files.push((file, namespace));
        let tree = item_tree(db, file);
        for &idx in tree.top_level() {
            let item = tree.item(idx);
            builder.namespaces[namespace]
                .items
                .entry(item.name.clone())
                .or_default()
                .push(ItemId {
                    project,
                    file,
                    item: idx,
                });
        }
        for (idx, item) in tree.items() {
            if item.kind != ItemKind::Function || item.name != "Main" {
                continue;
            }
            let counts = match item.parent {
                None => true,
                Some(parent) => item.is_static && tree.item(parent).kind != ItemKind::Interface,
            };
            if counts {
                mains.push(ItemId {
                    project,
                    file,
                    item: idx,
                });
            }
        }
        if tree.has_statements() {
            statement_files.push(file);
        }
    }
    let entry = project.entry(db);
    let entry_file = map_files
        .iter()
        .map(|&(file, _)| file)
        .find(|file| file.path(db) == entry);
    DefMap {
        namespaces: builder.namespaces,
        outermost,
        root,
        files: map_files,
        folder_problems: builder.folder_problems,
        mains,
        statement_files,
        kind: project.kind(db),
        entry_file,
    }
}

/// The parts of a [`DefMap`] that the folders of its files build.
struct Builder {
    /// The namespaces so far.
    namespaces: Arena<Namespace>,
    /// The namespace and first file of each folder seen, by its path.
    folders: BTreeMap<String, (NamespaceId, SourceFile)>,
    /// The problems found so far.
    folder_problems: Vec<FolderProblem>,
}

impl Builder {
    /// The namespace of the file at `path`, creating the namespaces of its
    /// folders on first sight. Files come by path, so the first file seen
    /// in a folder is its first file.
    fn namespace_of(&mut self, root: NamespaceId, path: &str, file: SourceFile) -> NamespaceId {
        let relative = path.strip_prefix("src/").unwrap_or(path);
        let mut folders: Vec<&str> = relative.split('/').collect();
        folders.pop();
        let mut namespace = root;
        let mut folder = String::from("src");
        for name in folders {
            folder.push('/');
            folder.push_str(name);
            if let Some(&(id, _)) = self.folders.get(&folder) {
                namespace = id;
                continue;
            }
            let pascal = pascal_case(name);
            if pascal.is_none() {
                self.folder_problems.push(FolderProblem::Invalid {
                    folder: folder.clone(),
                    file,
                });
            }
            // A folder that gives no name still gets a namespace, under a name
            // no code can write, so its files are checked like any other.
            let segment = pascal.unwrap_or_else(|| name.to_owned());
            let existing = self.namespaces[namespace].children.get(&segment).copied();
            let id = match existing {
                Some(id) => {
                    if let Some((other, &(_, other_file))) =
                        self.folders.iter().find(|(_, (it, _))| *it == id)
                    {
                        self.folder_problems.push(FolderProblem::Clash {
                            folder: folder.clone(),
                            other: other.clone(),
                            namespace: path_of(&self.namespaces, id),
                            file,
                            other_file,
                        });
                    }
                    id
                }
                None => add_child(&mut self.namespaces, namespace, segment),
            };
            self.folders.insert(folder.clone(), (id, file));
            namespace = id;
        }
        namespace
    }
}

impl Namespace {
    /// A namespace with nothing in it yet.
    fn new(name: String, parent: Option<NamespaceId>) -> Self {
        Self {
            name,
            parent,
            children: BTreeMap::new(),
            items: BTreeMap::new(),
        }
    }
}

/// Adds the namespace `name` inside `parent`.
fn add_child(namespaces: &mut Arena<Namespace>, parent: NamespaceId, name: String) -> NamespaceId {
    let id = namespaces.alloc(Namespace::new(name.clone(), Some(parent)));
    namespaces[parent].children.insert(name, id);
    id
}

/// `start` and the namespaces around it, from the innermost out.
fn enclosing(
    namespaces: &Arena<Namespace>,
    start: Option<NamespaceId>,
) -> impl Iterator<Item = NamespaceId> + '_ {
    std::iter::successors(start, |&id| namespaces[id].parent)
}

/// The full name of a namespace, its segments joined with `.`.
fn path_of(namespaces: &Arena<Namespace>, id: NamespaceId) -> String {
    let mut segments: Vec<&str> = enclosing(namespaces, Some(id))
        .map(|id| namespaces[id].name.as_str())
        .collect();
    segments.reverse();
    segments.join(".")
}

#[cfg(test)]
mod tests {
    use mora_span::{Project, ProjectKind};
    use salsa::Setter;

    use crate::test_db::{TestDb, app, codes, file_named, resolve};
    use crate::*;

    #[test]
    fn an_edit_inside_a_body_does_not_rebuild_the_def_map() {
        let mut db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "using UsersApi.Store;\nlet x = 1;\n"),
                (
                    "src/Store/UserStore.mr",
                    "internal class UserStore {\n    public Count(): int {\n        return 1;\n    }\n}\n",
                ),
            ],
        );
        assert_eq!(diagnostics(&db, project), &[]);
        db.take_executed();

        // The edit makes the file longer, so every range after it moves.
        let store = file_named(&db, project, "src/Store/UserStore.mr");
        let text = "internal class UserStore {\n    public Count(): int {\n        let n = 40 + 2;\n        \
                    return n;\n    }\n}\n";
        store.set_text(&mut db).to(text.into());
        assert_eq!(diagnostics(&db, project), &[]);
        let executed = db.take_executed();
        let ran = |query: &str| {
            executed
                .iter()
                .any(|key| key.starts_with(&format!("{query}(")))
        };
        assert!(ran("parse"), "{executed:?}");
        assert!(ran("lowered"), "{executed:?}");
        assert!(!ran("def_map"), "{executed:?}");
        assert!(!ran("imports"), "{executed:?}");

        // A new declaration changes the item tree, so the def map is rebuilt.
        store
            .set_text(&mut db)
            .to("internal class UserStore {}\ninternal class Cache {}\n".into());
        def_map(&db, project);
        let executed = db.take_executed();
        assert!(
            executed.iter().any(|key| key.starts_with("def_map(")),
            "{executed:?}"
        );
    }

    #[test]
    fn folders_give_namespaces() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", ""),
                ("src/Store/UserStore.mr", ""),
                ("src/http-client/Retry.mr", ""),
                ("src/http-client/auth/Token.mr", ""),
            ],
        );
        let map = def_map(&db, project);
        let namespace = |path: &str| {
            let file = file_named(&db, project, path);
            map.namespace_path(map.file_namespace(file).unwrap())
        };
        assert_eq!(namespace("src/Program.mr"), "UsersApi");
        assert_eq!(namespace("src/Store/UserStore.mr"), "UsersApi.Store");
        assert_eq!(namespace("src/http-client/Retry.mr"), "UsersApi.HttpClient");
        assert_eq!(
            namespace("src/http-client/auth/Token.mr"),
            "UsersApi.HttpClient.Auth"
        );
        assert_eq!(map.folder_problems(), &[]);
    }

    #[test]
    fn bad_and_clashing_folders_are_reported() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/2d/Shape.mr", ""),
                ("src/HttpClient/A.mr", ""),
                ("src/http-client/B.mr", ""),
            ],
        );
        let problems = def_map(&db, project).folder_problems();
        assert!(matches!(
            &problems[0],
            FolderProblem::Invalid { folder, .. } if folder == "src/2d"
        ));
        assert!(matches!(
            &problems[1],
            FolderProblem::Clash { folder, other, namespace, .. }
                if folder == "src/http-client"
                    && other == "src/HttpClient"
                    && namespace == "UsersApi.HttpClient"
        ));
        assert_eq!(codes(diagnostics(&db, project)), ["MR0121", "MR0121"]);
    }

    #[test]
    fn entry_points() {
        let db = TestDb::new();
        let entry = |files: &[(&str, &str)]| {
            let project = app(&db, files);
            (
                def_map(&db, project).entry_point(),
                codes(diagnostics(&db, project)),
            )
        };
        let (point, reported) = entry(&[("src/Program.mr", "let x = 1;\nHelper() {}\n")]);
        assert!(matches!(point, EntryPoint::Statements(_)), "{point:?}");
        assert!(reported.is_empty());

        // Only a function counts, not a type named `Main`.
        let (point, _) = entry(&[("src/Program.mr", "let x = 1;\nclass Main {}\n")]);
        assert!(matches!(point, EntryPoint::Statements(_)), "{point:?}");

        let (point, reported) = entry(&[("src/App.mr", "class App {\n    static Main() {}\n}\n")]);
        assert!(matches!(point, EntryPoint::Main(_)), "{point:?}");
        assert!(reported.is_empty());

        // An instance method named `Main` is not an entry point.
        let (point, _) = entry(&[("src/App.mr", "class App {\n    Main() {}\n}\n")]);
        assert_eq!(point, EntryPoint::None);

        let (point, reported) = entry(&[("src/Program.mr", "let x = 1;\nMain() {}\n")]);
        assert_eq!(point, EntryPoint::Conflict);
        assert_eq!(reported, ["MR0124"]);

        let (_, reported) = entry(&[("src/A.mr", "Main() {}\n"), ("src/B.mr", "Main() {}\n")]);
        assert_eq!(reported, ["MR0124"]);

        let (_, reported) = entry(&[
            ("src/Program.mr", "let x = 1;\n"),
            ("src/Util.mr", "let y = 2;\n"),
        ]);
        assert_eq!(reported, ["MR0125"]);

        let lib = db.project(
            "lib",
            ProjectKind::Lib,
            &[("src/Lib.mr", "let x = 1;\n")],
            Vec::new(),
        );
        assert_eq!(def_map(&db, lib).entry_point(), EntryPoint::None);
        assert_eq!(codes(diagnostics(&db, lib)), ["MR0125"]);
    }

    #[test]
    fn a_dotted_root_namespace_is_a_chain() {
        let db = TestDb::new();
        let file = db.file("src/Store/A.mr", "internal class A {}\n");
        let project = Project::new(
            &db,
            "users".into(),
            vec![file],
            "Acme.Users".into(),
            ProjectKind::Lib,
            "src/Program.mr".into(),
            Vec::new(),
        );
        let map = def_map(&db, project);
        assert_eq!(map.namespace(map.outermost()).name, "Acme");
        assert_eq!(map.namespace_path(map.root()), "Acme.Users");
        assert_eq!(
            resolve(&db, project, "src/Store/A.mr", "Acme.Users.Store.A"),
            Ok("Acme.Users.Store.A".into())
        );
    }
}
