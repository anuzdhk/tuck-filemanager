// SPDX-License-Identifier: GPL-3.0-or-later
//! UI-free core of the Strata file manager.
//!
//! Nothing in this crate depends on GTK. The application crate feeds it plain
//! data and gets plain data back, which keeps all of this unit-testable.

pub mod entry;
pub mod format;
pub mod group;
pub mod listing;
pub mod paths;
pub mod settings;
pub mod sort;
pub mod typeahead;

pub use entry::{EntryKind, EntryMeta};
