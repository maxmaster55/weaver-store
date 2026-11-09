use std::cell::RefCell;
use std::rc::Rc;

use weaver_store::{NodeKind, Store};

#[test]
fn computed_values_are_lazy_cached_and_minimal() {
    let store = Store::new();
    let a = store.signal(1);
    let b = store.signal(10);
    let sum = store.computed(move |s| s.get(a) + s.get(b));
    let parity = store.computed(move |s| s.get(sum) % 2 == 0);
    let label = store.computed(move |s| if s.get(parity) { "even" } else { "odd" }.to_string());
    assert_eq!(store.get(label), "odd");
    store.set(a, 3); // sum 13, still odd
    assert_eq!(store.get(label), "odd");
    let runs = |id: usize| store.inspect()[id].runs;
    assert_eq!(
        runs(label.into()),
        1,
        "label skipped: parity did not change"
    );
    assert_eq!(runs(sum.into()), 2);
    store.set(a, 3); // equal value: nothing at all
    assert_eq!(runs(sum.into()), 2);
}

#[test]
fn effects_run_once_per_batch_and_only_on_real_changes() {
    let store = Store::new();
    let first = store.signal("Ada".to_string());
    let last = store.signal("Lovelace".to_string());
    let full = store.computed(move |s| format!("{} {}", s.get(first), s.get(last)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let l = log.clone();
    store.effect(move |s| l.borrow_mut().push(s.get(full)));
    store.batch(|s| {
        s.set(first, "Grace".into());
        s.set(last, "Hopper".into());
    });
    store.set(first, "Grace".into());
    assert_eq!(*log.borrow(), ["Ada Lovelace", "Grace Hopper"]);
}

#[test]
fn dynamic_dependencies_follow_branches() {
    let store = Store::new();
    let show = store.signal(false);
    let detail = store.signal(1);
    let runs = Rc::new(RefCell::new(0));
    let r = runs.clone();
    store.effect(move |s| {
        *r.borrow_mut() += 1;
        if s.get(show) {
            s.get(detail);
        }
    });
    store.set(detail, 2); // not read yet
    assert_eq!(*runs.borrow(), 1);
    store.set(show, true);
    store.set(detail, 3);
    assert_eq!(*runs.borrow(), 3);
}

#[test]
fn effects_can_write_signals_and_be_disposed() {
    let store = Store::new();
    let celsius = store.signal(0.0);
    let fahrenheit = store.signal(0.0);
    let sync = store.effect(move |s| s.set(fahrenheit, s.get(celsius) * 9.0 / 5.0 + 32.0));
    assert_eq!(store.get(fahrenheit), 32.0);
    store.set(celsius, 100.0);
    assert_eq!(store.get(fahrenheit), 212.0);
    store.dispose(sync);
    store.set(celsius, 0.0);
    assert_eq!(store.get(fahrenheit), 212.0);
}

#[test]
fn undo_redo_with_transactions_and_labels() {
    let store = Store::new();
    let x = store.signal(0);
    let y = store.signal(0);
    store.name(x, "x");
    let hover = store.ephemeral(false);
    store.set(x, 1);
    store.set(hover, true); // not recorded
    store.transaction("move", |s| {
        s.set(x, 5);
        s.set(y, 7);
        s.set(x, 6); // merged into the same step
    });
    assert_eq!(store.history(), ["set x", "move"]);
    assert!(store.undo());
    assert_eq!((store.get(x), store.get(y)), (1, 0));
    assert_eq!(store.redo_label().as_deref(), Some("move"));
    assert!(store.redo());
    assert_eq!((store.get(x), store.get(y)), (6, 7));
    store.undo();
    store.set(y, 9); // a new change clears redo
    assert!(!store.can_redo());
    store.undo();
    store.undo();
    assert_eq!(store.get(x), 0);
    assert!(!store.undo());
    assert!(store.get(hover));
}

#[test]
fn snapshots_restore_as_one_undo_step() {
    let store = Store::new();
    let items = store.signal(vec![1, 2, 3]);
    let title = store.signal("Draft".to_string());
    let saved = store.snapshot();
    store.update(items, |v| v.clear());
    store.set(title, "Final".into());
    store.restore(&saved);
    assert_eq!(store.get(items), [1, 2, 3]);
    assert_eq!(store.undo_label().as_deref(), Some("restore"));
    store.undo();
    assert_eq!(store.get(title), "Final");
}

#[test]
fn undo_notifies_effects() {
    let store = Store::new();
    let n = store.signal(1);
    let seen = Rc::new(RefCell::new(0));
    let s2 = seen.clone();
    store.effect(move |s| *s2.borrow_mut() = s.get(n));
    store.set(n, 2);
    store.undo();
    assert_eq!(*seen.borrow(), 1);
}

#[test]
fn inspect_shows_the_graph() {
    let store = Store::new();
    let a = store.signal(1);
    let double = store.computed(move |s| s.get(a) * 2);
    store.name(double, "double");
    store.effect(move |s| {
        s.get(double);
    });
    let info = store.inspect();
    assert_eq!(info[2].kind, NodeKind::Effect);
    assert_eq!(info[2].deps, [1]);
    assert_eq!(info[1].deps, [0]);
    assert_eq!(info[1].name.as_deref(), Some("double"));
}

#[test]
#[should_panic(expected = "depends on itself")]
fn cycles_are_reported() {
    let store = Store::new();
    let cell: Rc<RefCell<Option<weaver_store::Computed<i32>>>> = Rc::new(RefCell::new(None));
    let inner = cell.clone();
    let c = store.computed(move |s| inner.borrow().map_or(0, |c| s.get(c)) + 1);
    *cell.borrow_mut() = Some(c);
    store.get(c);
}
