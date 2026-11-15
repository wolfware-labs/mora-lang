//! The read graph of a project: which constants, initializers and default
//! values read which, the cycles among them and the order evaluation runs
//! them in.
//!
//! Both walks of the graph keep their own stack instead of recursing, so a
//! long chain of constants that read each other cannot exhaust the stack.

use std::collections::{HashMap, HashSet};

use mora_hir::def_map;
use mora_span::{Db, Project, TextRange};

use super::scan::const_scan;
use super::{ConstKey, constants, defaults};

/// A cycle of reads: its first member (smallest `sort_key`) first, in the
/// order each reads the next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cycle {
    /// The keys of the cycle; the last one reads the first.
    pub(crate) members: Vec<ConstKey>,
    /// The first member's read of the second, or of itself in a cycle of
    /// one.
    pub(crate) read: TextRange,
}

/// The keys of a project that evaluation runs, in the order it runs them,
/// and the cycles among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstGraph {
    /// Every key, each after the keys it reads.
    order: Vec<ConstKey>,
    /// One cycle for each strongly connected component that has one.
    cycles: Vec<Cycle>,
    /// The index in `cycles` of the cycle of each key in a component that
    /// has one. Only looked up, never iterated.
    member_of: HashMap<ConstKey, usize>,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the interpreter that reads it comes next")
)]
impl ConstGraph {
    /// Every key of the project that evaluation runs, each after the keys
    /// it reads.
    pub(crate) fn order(&self) -> &[ConstKey] {
        &self.order
    }

    /// The cycle whose strongly connected component holds `key`.
    pub(crate) fn cycle_of(&self, key: ConstKey) -> Option<&Cycle> {
        self.member_of.get(&key).map(|&index| &self.cycles[index])
    }
}

/// The read graph of `project`. Its roots are the constants and then the
/// default values of each file, the files by path; initializers come in
/// through the reads. A read of a key of another project is a leaf: that
/// project evaluates it.
///
/// The keys of one strongly connected component follow each other in the
/// order of their `sort_key`, ties broken by the order the walk first
/// reached them in.
#[salsa::tracked(returns(ref))]
pub(crate) fn const_graph(db: &dyn Db, project: Project) -> ConstGraph {
    let mut tarjan = Tarjan {
        db,
        project,
        nodes: Vec::new(),
        ids: HashMap::new(),
        open: Vec::new(),
        graph: ConstGraph {
            order: Vec::new(),
            cycles: Vec::new(),
            member_of: HashMap::new(),
        },
    };
    for file in def_map(db, project).files() {
        for root in constants(db, file)
            .into_iter()
            .chain(defaults(db, project, file))
        {
            tarjan.walk(root);
        }
    }
    tarjan.graph
}

/// A key the walk has reached. Its id, its position in `Tarjan::nodes`, is
/// the order in which the walk reached it.
struct Node {
    /// The key.
    key: ConstKey,
    /// The keys of its project it reads, in the order of the text, each
    /// with the range of the read.
    reads: Vec<(ConstKey, TextRange)>,
    /// The smallest id it reaches among the nodes of open components.
    low: usize,
    /// Whether its component is still open.
    open: bool,
}

/// Tarjan's strongly connected components over the reads, with a stack of
/// its own in place of recursion.
struct Tarjan<'a> {
    /// The database.
    db: &'a dyn Db,
    /// The project of the graph.
    project: Project,
    /// Every node reached, by id.
    nodes: Vec<Node>,
    /// The id of each key reached. Only looked up, never iterated.
    ids: HashMap<ConstKey, usize>,
    /// The nodes of the components not yet complete, in the order reached.
    open: Vec<usize>,
    /// What the walk has found so far.
    graph: ConstGraph,
}

impl Tarjan<'_> {
    /// Gives `key` the next id and opens it.
    fn reach(&mut self, key: ConstKey) -> usize {
        let id = self.nodes.len();
        let reads = const_scan(self.db, self.project, key)
            .reads
            .iter()
            .filter(|&&(project, _, _)| project == self.project)
            .map(|&(_, read, range)| (read, range))
            .collect();
        self.ids.insert(key, id);
        self.nodes.push(Node {
            key,
            reads,
            low: id,
            open: true,
        });
        self.open.push(id);
        id
    }

    /// Walks every key that `root` reaches and the walk has not, closing
    /// each component once every key it reads is in a closed one.
    fn walk(&mut self, root: ConstKey) {
        if self.ids.contains_key(&root) {
            return;
        }
        // Each frame is a node and the position of its next read.
        let mut calls = vec![(self.reach(root), 0)];
        while let Some(&mut (node, ref mut next)) = calls.last_mut() {
            if let Some(&(read, _)) = self.nodes[node].reads.get(*next) {
                *next += 1;
                match self.ids.get(&read) {
                    None => {
                        let id = self.reach(read);
                        calls.push((id, 0));
                    }
                    Some(&id) if self.nodes[id].open => {
                        self.nodes[node].low = self.nodes[node].low.min(id);
                    }
                    Some(_) => {}
                }
                continue;
            }
            calls.pop();
            let low = self.nodes[node].low;
            if let Some(&(caller, _)) = calls.last() {
                self.nodes[caller].low = self.nodes[caller].low.min(low);
            }
            if low == node {
                self.close(node);
            }
        }
    }

    /// Closes the component whose first node is `first`: its keys join
    /// the order, and its cycle, when it has one, the cycles.
    fn close(&mut self, first: usize) {
        // `open` is in id order, and the component is its nodes from
        // `first` on.
        let at = self.open.partition_point(|&id| id < first);
        let mut members = self.open.split_off(at);
        for &id in &members {
            self.nodes[id].open = false;
        }
        // `members` is in id order, so a stable sort breaks ties by id.
        members.sort_by_cached_key(|&id| self.nodes[id].key.sort_key(self.db));
        self.graph
            .order
            .extend(members.iter().map(|&id| self.nodes[id].key));
        let start = members[0];
        let reads_itself = self.nodes[start]
            .reads
            .iter()
            .any(|&(read, _)| read == self.nodes[start].key);
        if members.len() == 1 && !reads_itself {
            return;
        }
        let cycle = self.cycle(start, &members);
        let index = self.graph.cycles.len();
        for &id in &members {
            self.graph.member_of.insert(self.nodes[id].key, index);
        }
        self.graph.cycles.push(cycle);
    }

    /// The cycle of the component `members` from `start`: depth first,
    /// along the first read in the order of the text that stays in the
    /// component, until a read returns to `start`.
    fn cycle(&self, start: usize, members: &[usize]) -> Cycle {
        // The members not yet on the path or left behind; a read of
        // anything else does not lead back to `start`.
        let mut unvisited: HashSet<usize> =
            members.iter().copied().filter(|&id| id != start).collect();
        // Each step is a node, the position of its next read, and the
        // range of the read that led to it.
        let mut path: Vec<(usize, usize, Option<TextRange>)> = vec![(start, 0, None)];
        while let Some(&mut (node, ref mut next, _)) = path.last_mut() {
            let Some(&(read, range)) = self.nodes[node].reads.get(*next) else {
                path.pop();
                continue;
            };
            *next += 1;
            let id = self.ids[&read];
            if id == start {
                let read = path.get(1).and_then(|&(_, _, via)| via).unwrap_or(range);
                let members = path.iter().map(|&(id, _, _)| self.nodes[id].key).collect();
                return Cycle { members, read };
            }
            if unvisited.remove(&id) {
                path.push((id, 0, Some(range)));
            }
        }
        let name = self.nodes[start].key.name(self.db, self.project);
        panic!("internal error evaluating {name}: no cycle leads back to it");
    }
}
