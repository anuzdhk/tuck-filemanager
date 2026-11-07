// SPDX-License-Identifier: GPL-3.0-or-later
//! The search bar: query field, scope (this folder / everywhere), filters
//! for kind, date and size, and "search contents". Every control is a real
//! button or drop-down, so everything is reachable by keyboard.

use std::rc::Rc;

use gtk::prelude::*;
use tuck_core::search::{DateFilter, Kind, Query};

#[derive(Clone)]
pub struct SearchBar {
    pub revealer: gtk::Revealer,
    pub entry: gtk::SearchEntry,
    here: gtk::ToggleButton,
    everywhere: gtk::ToggleButton,
    kind: gtk::DropDown,
    date: gtk::DropDown,
    size: gtk::DropDown,
    contents: gtk::ToggleButton,
    status: gtk::Label,
    results: gtk::Label,
}

/// (label, min bytes, max bytes)
const SIZES: &[(&str, Option<u64>, Option<u64>)] = &[
    ("Any Size", None, None),
    ("Under 100 KB", None, Some(100_000)),
    ("Over 1 MB", Some(1_000_000), None),
    ("Over 100 MB", Some(100_000_000), None),
    ("Over 1 GB", Some(1_000_000_000), None),
];

impl SearchBar {
    pub fn new() -> Self {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search files — try kind:pdf, size:>10mb, modified:week, tag:work")
            .hexpand(true)
            .search_delay(40)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Search")]);
        let here = gtk::ToggleButton::builder().label("This Folder").tooltip_text("Search in the current folder").build();
        let everywhere = gtk::ToggleButton::builder().label("Everywhere").tooltip_text("Search your whole home folder").active(true).build();
        here.set_group(Some(&everywhere));
        let scope = gtk::Box::builder().css_classes(["linked"]).build();
        scope.append(&here);
        scope.append(&everywhere);

        let mut kinds = vec!["Any Kind"];
        kinds.extend(Kind::FILTERS.iter().map(|k| k.label()));
        let kind = gtk::DropDown::from_strings(&kinds);
        kind.update_property(&[gtk::accessible::Property::Label("Kind")]);
        let mut dates = vec!["Any Time"];
        dates.extend(DateFilter::ALL.iter().map(|d| d.label()));
        let date = gtk::DropDown::from_strings(&dates);
        date.update_property(&[gtk::accessible::Property::Label("Date modified")]);
        let size = gtk::DropDown::from_strings(&SIZES.iter().map(|s| s.0).collect::<Vec<_>>());
        size.update_property(&[gtk::accessible::Property::Label("Size")]);
        let contents = gtk::ToggleButton::builder()
            .label("Contents")
            .tooltip_text("Also search inside text files")
            .build();

        let status = gtk::Label::builder().xalign(1.0).css_classes(["dim-label", "numeric", "caption"]).build();
        let results = gtk::Label::builder().xalign(1.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["numeric", "caption-heading"]).build();

        let top = gtk::Box::builder().spacing(8).build();
        top.append(&entry);
        top.append(&scope);
        let filters = gtk::Box::builder().spacing(8).build();
        filters.append(&kind);
        filters.append(&date);
        filters.append(&size);
        filters.append(&contents);
        let spacer = gtk::Box::builder().hexpand(true).build();
        filters.append(&spacer);
        filters.append(&results);
        filters.append(&status);
        let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).css_classes(["search-bar"]).build();
        col.append(&top);
        col.append(&filters);
        let revealer = gtk::Revealer::builder().child(&col).transition_type(gtk::RevealerTransitionType::SlideDown).build();
        SearchBar { revealer, entry, here, everywhere, kind, date, size, contents, status, results }
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        let f = Rc::new(f);
        let g = f.clone();
        self.entry.connect_search_changed(move |_| g());
        for b in [&self.here, &self.everywhere] {
            let g = f.clone();
            b.connect_toggled(move |b| {
                if b.is_active() {
                    g();
                }
            });
        }
        let g = f.clone();
        self.contents.connect_toggled(move |_| g());
        for d in [&self.kind, &self.date, &self.size] {
            let g = f.clone();
            d.connect_selected_notify(move |_| g());
        }
    }

    pub fn connect_stop(&self, f: impl Fn() + 'static) {
        self.entry.connect_stop_search(move |_| f());
    }

    pub fn is_open(&self) -> bool {
        self.revealer.reveals_child()
    }

    pub fn open(&self) {
        self.revealer.set_reveal_child(true);
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    pub fn close(&self) {
        self.revealer.set_reveal_child(false);
    }

    pub fn everywhere(&self) -> bool {
        self.everywhere.is_active()
    }

    pub fn set_contents(&self, on: bool) {
        self.contents.set_active(on);
    }

    pub fn set_everywhere(&self, on: bool) {
        if on { self.everywhere.set_active(true) } else { self.here.set_active(true) }
    }

    pub fn contents(&self) -> bool {
        self.contents.is_active()
    }

    pub fn set_status(&self, s: &str) {
        self.status.set_label(s);
    }

    /// The result count ("7116 results · 13.7 ms · 2 selected").
    pub fn set_results(&self, s: &str) {
        self.results.set_label(s);
    }

    /// The query from the text plus the filter controls.
    pub fn query(&self) -> Query {
        let mut q = Query::parse(&self.entry.text(), &tuck_core::paths::home());
        if let Some(k) = (self.kind.selected() as usize).checked_sub(1).and_then(|i| Kind::FILTERS.get(i))
            && !q.kinds.contains(k) {
                q.kinds.push(*k);
            }
        if let Some(d) = (self.date.selected() as usize).checked_sub(1).and_then(|i| DateFilter::ALL.get(i)) {
            q.modified.get_or_insert(*d);
        }
        if let Some((_, min, max)) = SIZES.get(self.size.selected() as usize) {
            if q.min_size.is_none() {
                q.min_size = *min;
            }
            if q.max_size.is_none() {
                q.max_size = *max;
            }
        }
        q
    }
}
