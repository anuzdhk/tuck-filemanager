// SPDX-License-Identifier: GPL-3.0-or-later
//! Development tool: render the app icon SVG to PNGs for packaging.
//!   cargo run -p tuck-core --example render_icon -- data/icons/app
use std::path::Path;

fn main() {
    let dir = Path::new(&std::env::args().nth(1).expect("icon folder")).to_path_buf();
    let svg = std::fs::read(dir.join("io.github.anuzdhk.Tuck.svg")).expect("read svg");
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default()).expect("parse svg");
    for size in [16u32, 24, 32, 48, 64, 128, 256, 512] {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("pixmap");
        let scale = size as f32 / tree.size().width();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        let out = dir.join(format!("hicolor/{size}x{size}/apps"));
        std::fs::create_dir_all(&out).expect("mkdir");
        pixmap.save_png(out.join("io.github.anuzdhk.Tuck.png")).expect("save png");
    }
    println!("rendered 8 sizes");
}
