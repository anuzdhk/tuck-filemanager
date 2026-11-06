// Embed the bundled icon SVGs (see data/icons/README.md) into the binary.
use std::fmt::Write;
use std::path::Path;

fn list(dir: &Path, out: &mut String, name: &str) {
    writeln!(out, "pub static {name}: &[(&str, &str)] = &[").unwrap();
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "svg")).collect();
    files.sort();
    for f in files {
        let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
        writeln!(out, "    ({stem:?}, include_str!({:?})),", f.canonicalize().unwrap()).unwrap();
    }
    writeln!(out, "];").unwrap();
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/icons");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out = String::new();
    list(&root.join("tabler-symbolic"), &mut out, "TABLER");
    list(&root.join("material"), &mut out, "MATERIAL");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("icons.rs"), out).unwrap();
}
