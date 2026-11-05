// SPDX-License-Identifier: GPL-3.0-or-later
//! UI-free core of the Kivo file manager.
//!
//! Nothing in this crate depends on GTK. The application crate feeds it plain
//! data and gets plain data back, which keeps all of this unit-testable.

pub mod archive;
pub mod entry;
pub mod format;
pub mod group;
pub mod listing;
pub mod names;
pub mod ops;
pub mod paths;
pub mod perms;
pub mod preview;
pub mod search;
pub mod settings;
pub mod sort;
pub mod terminal;
pub mod thumbs;
pub mod trash;
pub mod typeahead;
pub mod undo;

pub use entry::{EntryKind, EntryMeta};
