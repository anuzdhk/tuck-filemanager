# Bundled icons

- `tabler/`: [Tabler Icons](https://github.com/tabler/tabler-icons) outline set, MIT
  (`LICENSES/tabler-icons-MIT.txt`). Unmodified.
- `tabler-symbolic/`: the same icons with strokes converted to filled outlines
  so GTK can recolour them (`cargo run -p kivo-core --example convert_icons`).
- `material/`: [Material Symbols](https://github.com/google/material-design-icons)
  Rounded, Apache-2.0 (`LICENSES/material-symbols-Apache-2.0.txt`). Unmodified.

Kivo's folder and document icons are original artwork generated in code
(`crates/kivo-core/src/icons.rs`); emblems reuse the Tabler glyphs above.
