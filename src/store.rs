use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::history::{Change, CloneFn, History, Snapshot, Step};

type EqFn = fn(&dyn Any, &dyn Any) -> bool;
type ComputeFn = Rc<dyn Fn(&Store) -> Box<dyn Any>>;
type EffectFn = Rc<RefCell<dyn FnMut(&Store)>>;

fn clone_of<T: Clone + 'static>(v: &dyn Any) -> Box<dyn Any> {
    Box::new(v.downcast_ref::<T>().expect("type matches").clone())
}

fn eq_of<T: PartialEq + 'static>(a: &dyn Any, b: &dyn Any) -> bool {
    a.downcast_ref::<T>() == b.downcast_ref::<T>()
}

fn never_eq(_: &dyn Any, _: &dyn Any) -> bool {
    false
}

fn no_clone(_: &dyn Any) -> Box<dyn Any> {
    Box::new(())
}

/// A piece of state. A small `Copy` handle: read it with
/// [`Store::get`], change it with [`Store::set`].
pub struct Signal<T> {
    id: usize,
    _t: PhantomData<fn() -> T>,
}

/// A value derived from signals (or other computed values). Lazy and
/// cached: it recomputes only when read after something it used changed.
pub struct Computed<T> {
    id: usize,
    _t: PhantomData<fn() -> T>,
}

/// A side effect that re-runs when what it read changes. Stop it with
/// [`Store::dispose`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Effect {
    id: usize,
}

macro_rules! handle_impls {
    ($name:ident) => {
        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<T> Copy for $name<T> {}
        impl<T> PartialEq for $name<T> {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl<T> Eq for $name<T> {}
        impl<T> std::hash::Hash for $name<T> {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.id.hash(state)
            }
        }
        impl<T> fmt::Debug for $name<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.id)
            }
        }
        impl<T> Read<T> for $name<T> {
            fn node(&self) -> usize {
                self.id
            }
        }
    };
}
handle_impls!(Signal);
handle_impls!(Computed);

/// Something [`Store::get`] can read: a [`Signal`] or a [`Computed`].
pub trait Read<T> {
    #[doc(hidden)]
    fn node(&self) -> usize;
}

/// What kind of node an entry in [`Store::inspect`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A signal.
    Signal,
    /// A computed value.
    Computed,
    /// An effect.
    Effect,
}

/// Debug information about one node, from [`Store::inspect`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    /// Its index in the store.
    pub id: usize,
    /// The name given with [`Store::name`], if any.
    pub name: Option<String>,
    /// Signal, computed or effect.
    pub kind: NodeKind,
    /// Bumped every time its value actually changes.
    pub version: u64,
    /// The nodes it read last time it ran.
    pub deps: Vec<usize>,
    /// How many times it has been computed or run.
    pub runs: u64,
}

enum Kind {
    Signal { tracked: bool },
    Computed { f: ComputeFn, dirty: bool },
    Effect { f: EffectFn, disposed: bool },
}

struct Node {
    kind: Kind,
    value: Option<Box<dyn Any>>,
    version: u64,
    deps: Vec<(usize, u64)>,
    subs: Vec<usize>,
    eq: EqFn,
    clone: CloneFn,
    name: Option<String>,
    runs: u64,
}

/// Holds signals, computed values and effects, plus the undo history.
///
/// A store lives on one thread (like most UI state). Handles are cheap
/// `Copy` values you can keep in your own structs and closures.
#[derive(Default)]
pub struct Store {
    nodes: RefCell<Vec<Node>>,
    tracking: RefCell<Vec<Vec<usize>>>,
    evaluating: RefCell<Vec<usize>>,
    batch: Cell<usize>,
    flushing: Cell<bool>,
    pending: RefCell<BTreeSet<usize>>,
    history: RefCell<History>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("nodes", &self.nodes.borrow().len())
            .field("can_undo", &self.can_undo())
            .finish()
    }
}

impl Store {
    /// An empty store.
    pub fn new() -> Self {
        Store::default()
    }

    fn add(&self, kind: Kind, value: Option<Box<dyn Any>>, eq: EqFn, clone: CloneFn) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push(Node {
            kind,
            value,
            version: 0,
            deps: Vec::new(),
            subs: Vec::new(),
            eq,
            clone,
            name: None,
            runs: 0,
        });
        nodes.len() - 1
    }

    // ------------------------------------------------------- creating --

    /// A new signal holding `value`. Its changes are undoable and it is
    /// part of snapshots.
    pub fn signal<T: Clone + PartialEq + 'static>(&self, value: T) -> Signal<T> {
        self.make_signal(value, true)
    }

    /// A signal kept out of undo history and snapshots, for things like
    /// hover state, scroll position or a loading flag.
    pub fn ephemeral<T: Clone + PartialEq + 'static>(&self, value: T) -> Signal<T> {
        self.make_signal(value, false)
    }

    fn make_signal<T: Clone + PartialEq + 'static>(&self, value: T, tracked: bool) -> Signal<T> {
        let id = self.add(
            Kind::Signal { tracked },
            Some(Box::new(value)),
            eq_of::<T>,
            clone_of::<T>,
        );
        Signal {
            id,
            _t: PhantomData,
        }
    }

    /// A value computed from other values. `f` reads them with
    /// `store.get`, and those reads are tracked automatically.
    pub fn computed<T, F>(&self, f: F) -> Computed<T>
    where
        T: Clone + PartialEq + 'static,
        F: Fn(&Store) -> T + 'static,
    {
        let f: ComputeFn = Rc::new(move |s| Box::new(f(s)));
        let id = self.add(
            Kind::Computed { f, dirty: true },
            None,
            eq_of::<T>,
            clone_of::<T>,
        );
        Computed {
            id,
            _t: PhantomData,
        }
    }

    /// Run `f` now, and again whenever something it read changes (once
    /// per batch). Effects may set signals; the store settles before
    /// returning.
    pub fn effect(&self, f: impl FnMut(&Store) + 'static) -> Effect {
        let f: EffectFn = Rc::new(RefCell::new(f));
        let id = self.add(
            Kind::Effect { f, disposed: false },
            None,
            never_eq,
            no_clone,
        );
        self.run_effect(id);
        self.flush();
        Effect { id }
    }

    /// Stop an effect.
    pub fn dispose(&self, effect: Effect) {
        let mut nodes = self.nodes.borrow_mut();
        let deps = std::mem::take(&mut nodes[effect.id].deps);
        for (d, _) in deps {
            nodes[d].subs.retain(|s| *s != effect.id);
        }
        if let Kind::Effect { disposed, .. } = &mut nodes[effect.id].kind {
            *disposed = true;
        }
    }

    /// Give a node a name for [`Store::inspect`] and undo labels.
    pub fn name(&self, node: impl Into<usize>, name: &str) {
        self.nodes.borrow_mut()[node.into()].name = Some(name.to_owned());
    }

    // -------------------------------------------------------- reading --

    /// The current value of a signal or computed value. Inside a computed
    /// value or an effect, this also records the dependency.
    pub fn get<T: Clone + 'static>(&self, r: impl Read<T>) -> T {
        let id = r.node();
        if let Some(frame) = self.tracking.borrow_mut().last_mut() {
            frame.push(id);
        }
        self.refresh(id);
        let nodes = self.nodes.borrow();
        nodes[id]
            .value
            .as_ref()
            .and_then(|v| v.downcast_ref::<T>())
            .cloned()
            .expect("a handle always matches its store's value type")
    }

    /// Read without recording a dependency, for code inside a computed
    /// value or effect that should not re-run when this changes.
    pub fn peek<T: Clone + 'static>(&self, r: impl Read<T>) -> T {
        self.tracking.borrow_mut().push(Vec::new());
        let v = self.get(r);
        self.tracking.borrow_mut().pop();
        v
    }

    /// Bring a computed node up to date.
    fn refresh(&self, id: usize) {
        let (stale, has_value, deps) = {
            let nodes = self.nodes.borrow();
            match &nodes[id].kind {
                Kind::Computed { dirty, .. } => {
                    (*dirty, nodes[id].value.is_some(), nodes[id].deps.clone())
                }
                _ => return,
            }
        };
        if !stale && has_value {
            return;
        }
        if has_value && !self.deps_changed(&deps) {
            if let Kind::Computed { dirty, .. } = &mut self.nodes.borrow_mut()[id].kind {
                *dirty = false;
            }
            return;
        }
        self.evaluate(id);
    }

    fn deps_changed(&self, deps: &[(usize, u64)]) -> bool {
        deps.iter().any(|&(d, seen)| {
            self.refresh(d);
            self.nodes.borrow()[d].version != seen
        })
    }

    fn evaluate(&self, id: usize) {
        if self.evaluating.borrow().contains(&id) {
            panic!("weaver-store: a computed value depends on itself (node {id})");
        }
        let f = match &self.nodes.borrow()[id].kind {
            Kind::Computed { f, .. } => f.clone(),
            _ => return,
        };
        self.evaluating.borrow_mut().push(id);
        self.tracking.borrow_mut().push(Vec::new());
        let value = f(self);
        let reads = self.tracking.borrow_mut().pop().unwrap_or_default();
        self.evaluating.borrow_mut().pop();

        let mut nodes = self.nodes.borrow_mut();
        Self::resubscribe(&mut nodes, id, reads);
        let node = &mut nodes[id];
        let changed = match &node.value {
            Some(old) => !(node.eq)(old.as_ref(), value.as_ref()),
            None => true,
        };
        if changed {
            node.value = Some(value);
            node.version += 1;
        }
        node.runs += 1;
        if let Kind::Computed { dirty, .. } = &mut node.kind {
            *dirty = false;
        }
    }

    fn resubscribe(nodes: &mut [Node], id: usize, reads: Vec<usize>) {
        let old = std::mem::take(&mut nodes[id].deps);
        for (d, _) in old {
            nodes[d].subs.retain(|s| *s != id);
        }
        let mut seen = BTreeSet::new();
        let deps: Vec<(usize, u64)> = reads
            .into_iter()
            .filter(|d| *d != id && seen.insert(*d))
            .map(|d| (d, nodes[d].version))
            .collect();
        for (d, _) in &deps {
            nodes[*d].subs.push(id);
        }
        nodes[id].deps = deps;
    }

    fn run_effect(&self, id: usize) {
        let f = match &self.nodes.borrow()[id].kind {
            Kind::Effect { f, disposed: false } => f.clone(),
            _ => return,
        };
        self.tracking.borrow_mut().push(Vec::new());
        // Changes made by the effect are flushed by the caller, never from
        // inside the effect itself.
        self.batch.set(self.batch.get() + 1);
        (f.borrow_mut())(self);
        self.batch.set(self.batch.get() - 1);
        let reads = self.tracking.borrow_mut().pop().unwrap_or_default();
        let mut nodes = self.nodes.borrow_mut();
        if matches!(
            nodes[id].kind,
            Kind::Effect {
                disposed: false,
                ..
            }
        ) {
            Self::resubscribe(&mut nodes, id, reads);
        }
        nodes[id].runs += 1;
    }

    // ------------------------------------------------------- changing --

    /// Change a signal. Nothing happens if the value is equal to the
    /// current one.
    pub fn set<T: Clone + PartialEq + 'static>(&self, signal: Signal<T>, value: T) {
        self.set_any(signal.id, Box::new(value), true);
        self.flush();
    }

    /// Change a signal in place.
    ///
    /// ```
    /// let store = weaver_store::Store::new();
    /// let todos = store.signal(vec!["write docs"]);
    /// store.update(todos, |t| t.push("ship it"));
    /// assert_eq!(store.get(todos).len(), 2);
    /// ```
    pub fn update<T: Clone + PartialEq + 'static>(
        &self,
        signal: Signal<T>,
        f: impl FnOnce(&mut T),
    ) {
        let mut v = self.peek(signal);
        f(&mut v);
        self.set(signal, v);
    }

    fn set_any(&self, id: usize, value: Box<dyn Any>, record: bool) -> bool {
        let change = {
            let mut nodes = self.nodes.borrow_mut();
            let node = &mut nodes[id];
            let tracked = matches!(node.kind, Kind::Signal { tracked: true });
            let old = node.value.as_ref().expect("signals always hold a value");
            if (node.eq)(old.as_ref(), value.as_ref()) {
                return false;
            }
            let change = (record && tracked).then(|| Change {
                id,
                old: (node.clone)(old.as_ref()),
                new: (node.clone)(value.as_ref()),
                clone: node.clone,
            });
            node.value = Some(value);
            node.version += 1;
            change
        };
        if let Some(change) = change {
            let label = self.label_of(id);
            let mut h = self.history.borrow_mut();
            match &mut h.open {
                Some(step) => step.record(change),
                None => h.push(Step {
                    label: format!("set {label}"),
                    changes: vec![change],
                }),
            }
        }
        self.mark(id);
        true
    }

    fn label_of(&self, id: usize) -> String {
        self.nodes.borrow()[id]
            .name
            .clone()
            .unwrap_or_else(|| format!("#{id}"))
    }

    fn mark(&self, id: usize) {
        let subs = self.nodes.borrow()[id].subs.clone();
        for s in subs {
            let recurse = {
                let mut nodes = self.nodes.borrow_mut();
                match &mut nodes[s].kind {
                    Kind::Computed { dirty, .. } => !std::mem::replace(dirty, true),
                    Kind::Effect {
                        disposed: false, ..
                    } => {
                        self.pending.borrow_mut().insert(s);
                        false
                    }
                    _ => false,
                }
            };
            if recurse {
                self.mark(s);
            }
        }
    }

    /// Make several changes; effects run once at the end.
    pub fn batch<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        self.batch.set(self.batch.get() + 1);
        let r = f(self);
        self.batch.set(self.batch.get() - 1);
        self.flush();
        r
    }

    fn flush(&self) {
        if self.batch.get() > 0 || self.flushing.get() {
            return;
        }
        self.flushing.set(true);
        let mut rounds = 0;
        loop {
            let ids: Vec<usize> = std::mem::take(&mut *self.pending.borrow_mut())
                .into_iter()
                .collect();
            if ids.is_empty() {
                break;
            }
            rounds += 1;
            if rounds > 100 {
                self.flushing.set(false);
                panic!("weaver-store: effects keep changing each other's inputs (100 rounds)");
            }
            for id in ids {
                let deps = self.nodes.borrow()[id].deps.clone();
                if self.deps_changed(&deps) {
                    self.run_effect(id);
                }
            }
        }
        self.flushing.set(false);
    }

    // -------------------------------------------------------- history --

    /// Make several changes as one undo step called `label`.
    ///
    /// ```
    /// let store = weaver_store::Store::new();
    /// let (x, y) = (store.signal(0), store.signal(0));
    /// store.transaction("move", |s| {
    ///     s.set(x, 10);
    ///     s.set(y, 20);
    /// });
    /// assert_eq!(store.undo_label().as_deref(), Some("move"));
    /// store.undo();
    /// assert_eq!((store.get(x), store.get(y)), (0, 0));
    /// ```
    pub fn transaction<R>(&self, label: &str, f: impl FnOnce(&Store) -> R) -> R {
        {
            let mut h = self.history.borrow_mut();
            h.depth += 1;
            if h.open.is_none() {
                h.open = Some(Step {
                    label: label.to_owned(),
                    changes: Vec::new(),
                });
            }
        }
        let r = self.batch(f);
        let mut h = self.history.borrow_mut();
        h.depth -= 1;
        if h.depth == 0 {
            if let Some(step) = h.open.take() {
                h.push(step);
            }
        }
        r
    }

    /// Undo the last step. Returns false if there was nothing to undo.
    pub fn undo(&self) -> bool {
        self.travel(true)
    }

    /// Redo the last undone step. Returns false if there was nothing to
    /// redo.
    pub fn redo(&self) -> bool {
        self.travel(false)
    }

    fn travel(&self, back: bool) -> bool {
        let step = {
            let mut h = self.history.borrow_mut();
            if h.open.is_some() {
                return false;
            }
            match if back { h.undo.pop() } else { h.redo.pop() } {
                Some(s) => s,
                None => return false,
            }
        };
        self.batch(|s| {
            let changes: Box<dyn Iterator<Item = &Change>> = if back {
                Box::new(step.changes.iter().rev())
            } else {
                Box::new(step.changes.iter())
            };
            for c in changes {
                let v = if back { &c.old } else { &c.new };
                s.set_any(c.id, (c.clone)(v.as_ref()), false);
            }
        });
        let mut h = self.history.borrow_mut();
        if back {
            h.redo.push(step);
        } else {
            h.undo.push(step);
        }
        true
    }

    /// True if there is something to undo.
    pub fn can_undo(&self) -> bool {
        !self.history.borrow().undo.is_empty()
    }

    /// True if there is something to redo.
    pub fn can_redo(&self) -> bool {
        !self.history.borrow().redo.is_empty()
    }

    /// The label of the step [`Store::undo`] would undo, for an
    /// "Undo move" menu item.
    pub fn undo_label(&self) -> Option<String> {
        self.history.borrow().undo.last().map(|s| s.label.clone())
    }

    /// The label of the step [`Store::redo`] would redo.
    pub fn redo_label(&self) -> Option<String> {
        self.history.borrow().redo.last().map(|s| s.label.clone())
    }

    /// Labels of every undo step, oldest first.
    pub fn history(&self) -> Vec<String> {
        self.history
            .borrow()
            .undo
            .iter()
            .map(|s| s.label.clone())
            .collect()
    }

    /// Forget all undo and redo steps.
    pub fn clear_history(&self) {
        let mut h = self.history.borrow_mut();
        h.undo.clear();
        h.redo.clear();
    }

    /// Keep at most `steps` undo steps (default 200).
    pub fn set_history_limit(&self, steps: usize) {
        let mut h = self.history.borrow_mut();
        h.limit = steps.max(1);
        while h.undo.len() > h.limit {
            h.undo.remove(0);
        }
    }

    /// The value of every (non-ephemeral) signal right now.
    pub fn snapshot(&self) -> Snapshot {
        let nodes = self.nodes.borrow();
        Snapshot {
            values: nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| matches!(n.kind, Kind::Signal { tracked: true }))
                .map(|(i, n)| {
                    (
                        i,
                        (n.clone)(n.value.as_ref().expect("value").as_ref()),
                        n.clone,
                    )
                })
                .collect(),
        }
    }

    /// Put every signal back to a snapshot, as one undo step.
    pub fn restore(&self, snapshot: &Snapshot) {
        self.transaction("restore", |s| {
            for (id, v, clone) in &snapshot.values {
                s.set_any(*id, clone(v.as_ref()), true);
            }
        });
    }

    /// Every node with its name, kind, version, dependencies and run
    /// count: handy for debugging and dev tools.
    pub fn inspect(&self) -> Vec<NodeInfo> {
        self.nodes
            .borrow()
            .iter()
            .enumerate()
            .map(|(id, n)| NodeInfo {
                id,
                name: n.name.clone(),
                kind: match n.kind {
                    Kind::Signal { .. } => NodeKind::Signal,
                    Kind::Computed { .. } => NodeKind::Computed,
                    Kind::Effect { .. } => NodeKind::Effect,
                },
                version: n.version,
                deps: n.deps.iter().map(|(d, _)| *d).collect(),
                runs: n.runs,
            })
            .collect()
    }
}

impl<T> From<Signal<T>> for usize {
    fn from(s: Signal<T>) -> usize {
        s.id
    }
}

impl<T> From<Computed<T>> for usize {
    fn from(c: Computed<T>) -> usize {
        c.id
    }
}

impl From<Effect> for usize {
    fn from(e: Effect) -> usize {
        e.id
    }
}
