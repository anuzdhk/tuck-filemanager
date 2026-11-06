// SPDX-License-Identifier: GPL-3.0-or-later
//! Development tool: ask GitHub for the latest Tuck release.
//!   cargo run -p tuck-core --example check_update
fn main() {
    match tuck_core::update::latest() {
        Ok(r) => println!("latest {} (running {}), newer: {}, {} assets", r.tag, tuck_core::update::CURRENT, tuck_core::update::is_newer(&r.tag, tuck_core::update::CURRENT), r.assets.len()),
        Err(e) => println!("error: {e}"),
    }
}
