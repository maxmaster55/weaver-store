//! # weaver-store
//!
//! Reactive state for apps, games and tools. Keep your state in
//! [`Signal`]s, derive values with [`Computed`], react with effects, and
//! get undo/redo and snapshots for free.
//!
//! ```
//! use weaver_store::Store;
//!
//! let store = Store::new();
//! let price = store.signal(4.0);
//! let qty = store.signal(3);
//! let total = store.computed(move |s| s.get(price) * s.get(qty) as f64);
//!
//! assert_eq!(store.get(total), 12.0);
//! store.set(qty, 5);
//! assert_eq!(store.get(total), 20.0); // recomputed only when read
//!
//! store.undo();
//! assert_eq!(store.get(qty), 3);
//! store.redo();
//! assert_eq!(store.get(qty), 5);
//! ```
//!
//! Computed values are lazy and cached, effects run once per batch, and
//! nothing re-runs when an upstream change doesn't change its result.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod history;
mod store;

pub use history::Snapshot;
pub use store::{Computed, Effect, NodeInfo, NodeKind, Read, Signal, Store};

/// The README examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
