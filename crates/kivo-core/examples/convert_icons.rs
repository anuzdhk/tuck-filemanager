// SPDX-License-Identifier: GPL-3.0-or-later
//! Development tool: turn Tabler's stroke-based outline icons into filled
//! outlines, so GTK can recolour them as symbolic icons (GTK fills every
//! shape of a symbolic SVG, which turns strokes into blobs).
//!
//!   cargo run -p kivo-core --example convert_icons -- data/icons/tabler data/icons/tabler-symbolic
//!
//! The shapes are unchanged: each stroke is replaced by its exact outline at
//! the original width, caps and joins.

use std::fmt::Write as _;
use std::path::Path;

use usvg::tiny_skia_path::{PathSegment, Transform};

fn segments(path: &usvg::tiny_skia_path::Path, out: &mut String) {
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => write!(out, "M{:.3} {:.3}", p.x, p.y),
            PathSegment::LineTo(p) => write!(out, "L{:.3} {:.3}", p.x, p.y),
            PathSegment::QuadTo(a, p) => write!(out, "Q{:.3} {:.3} {:.3} {:.3}", a.x, a.y, p.x, p.y),
            PathSegment::CubicTo(a, b, p) => write!(out, "C{:.3} {:.3} {:.3} {:.3} {:.3} {:.3}", a.x, a.y, b.x, b.y, p.x, p.y),
            PathSegment::Close => write!(out, "Z"),
        }
        .expect("write to string");
    }
}

fn walk(group: &usvg::Group, d: &mut String) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => walk(g, d),
            usvg::Node::Path(p) => {
                let t: Transform = p.abs_transform();
                if let Some(stroke) = p.stroke()
                    && let Some(outline) = p.data().stroke(&stroke.to_tiny_skia(), 4.0)
                        && let Some(o) = outline.transform(t) {
                            segments(&o, d);
                        }
                if p.fill().is_some()
                    && let Some(o) = p.data().clone().transform(t) {
                        segments(&o, d);
                    }
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (src, dst) = (Path::new(&args[1]), Path::new(&args[2]));
    std::fs::create_dir_all(dst).expect("create output folder");
    let mut n = 0;
    let mut names: Vec<_> = std::fs::read_dir(src).expect("read icons").flatten().map(|e| e.path()).collect();
    names.sort();
    for f in names {
        if f.extension().is_none_or(|e| e != "svg") {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("read svg");
        let tree = usvg::Tree::from_str(&text, &usvg::Options::default()).expect("parse svg");
        let mut d = String::new();
        walk(tree.root(), &mut d);
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\"><!-- Tabler Icons (MIT), strokes converted to outlines for GTK --><path fill=\"#000\" d=\"{d}\"/></svg>\n"
        );
        std::fs::write(dst.join(f.file_name().expect("file name")), svg).expect("write svg");
        n += 1;
    }
    println!("converted {n} icons");
}
