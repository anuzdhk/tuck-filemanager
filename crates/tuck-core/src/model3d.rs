// SPDX-License-Identifier: GPL-3.0-or-later
//! 3D model previews: load the triangles of a glTF/GLB, OBJ, STL or binary
//! FBX file and draw them with a small software renderer (flat shading,
//! depth buffer, supersampled edges). No GPU or extra libraries needed, so
//! it works the same for thumbnails on worker threads and in Quick Look.

use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

pub const EXTENSIONS: &[&str] = &["glb", "gltf", "obj", "stl", "fbx"];

/// Files bigger than this aren't loaded (a preview isn't worth the memory).
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_model(path: &Path, mime: &str) -> bool {
    EXTENSIONS.contains(&ext_of(path).as_str())
        || matches!(mime, "model/gltf-binary" | "model/gltf+json" | "model/obj" | "model/stl" | "model/x-fbx")
}

pub fn load(path: &Path) -> Result<Mesh, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_BYTES {
        return Err("This model is too big to preview.".into());
    }
    let mesh = match ext_of(path).as_str() {
        "glb" | "gltf" => gltf_mesh(path)?,
        "obj" => obj_mesh(&std::fs::read_to_string(path).map_err(|e| e.to_string())?),
        "stl" => stl_mesh(&std::fs::read(path).map_err(|e| e.to_string())?)?,
        "fbx" => fbx_mesh(&std::fs::read(path).map_err(|e| e.to_string())?)?,
        _ => return Err("Not a 3D model this preview knows.".into()),
    };
    if mesh.triangles.is_empty() {
        return Err("This model has no triangles to show.".into());
    }
    Ok(mesh)
}

// ------------------------------------------------------------------- OBJ

/// Vertices and faces (polygons become fans); everything else is ignored.
pub fn obj_mesh(text: &str) -> Mesh {
    let mut m = Mesh::default();
    let mut face: Vec<u32> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let v: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if let [x, y, z] = v[..] {
                    m.positions.push([x, y, z]);
                }
            }
            Some("f") => {
                face.clear();
                let n = m.positions.len() as i64;
                for tok in it {
                    // "7", "7/1", "7//3", "-1/…": the first number, 1-based or relative.
                    let Some(i) = tok.split('/').next().and_then(|s| s.parse::<i64>().ok()) else { continue };
                    let i = if i < 0 { n + i } else { i - 1 };
                    if (0..n).contains(&i) {
                        face.push(i as u32);
                    }
                }
                for k in 1..face.len().saturating_sub(1) {
                    m.triangles.push([face[0], face[k], face[k + 1]]);
                }
            }
            _ => {}
        }
    }
    m
}

// ------------------------------------------------------------------- STL

/// Binary or ASCII STL. STL is Z-up (3D printing); turned Y-up here.
pub fn stl_mesh(data: &[u8]) -> Result<Mesh, String> {
    let mut m = Mesh::default();
    let mut push = |v: [f32; 3]| {
        m.positions.push([v[0], v[2], -v[1]]);
    };
    let binary_len = data.get(80..84).map(|b| 84 + 50 * u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize);
    if binary_len == Some(data.len()) {
        for tri in data[84..].as_chunks::<50>().0 {
            for k in 0..3 {
                let f = |j: usize| f32::from_le_bytes(tri[12 + k * 12 + j * 4..][..4].try_into().expect("4 bytes"));
                push([f(0), f(1), f(2)]);
            }
        }
    } else {
        let text = std::str::from_utf8(data).map_err(|_| "This STL file is damaged.".to_string())?;
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some("vertex") {
                let v: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if let [x, y, z] = v[..] {
                    push([x, y, z]);
                }
            }
        }
    }
    let n = m.positions.len() as u32 / 3;
    m.triangles = (0..n).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();
    Ok(m)
}

// ------------------------------------------------------------------ glTF

fn gltf_mesh(path: &Path) -> Result<Mesh, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let g = gltf::Gltf::from_slice(&bytes).map_err(|e| format!("This glTF file couldn't be read: {e}"))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for b in g.buffers() {
        let data = match b.source() {
            gltf::buffer::Source::Bin => g.blob.clone().unwrap_or_default(),
            gltf::buffer::Source::Uri(uri) => {
                if let Some(rest) = uri.strip_prefix("data:") {
                    use base64::Engine;
                    let b64 = rest.split_once(";base64,").map(|(_, d)| d).ok_or("An embedded buffer isn't base64.")?;
                    base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?
                } else {
                    let file = dir.join(percent_decode(uri));
                    // Only files beside the model, never elsewhere on disk.
                    if !file.starts_with(dir) || uri.contains("..") {
                        return Err("The model points outside its folder.".into());
                    }
                    std::fs::read(&file).map_err(|_| format!("The model's data file “{uri}” is missing."))?
                }
            }
        };
        buffers.push(data);
    }
    let mut m = Mesh::default();
    let mut add = |mesh: gltf::Mesh, xf: &[[f32; 4]; 4]| {
        for p in mesh.primitives() {
            if p.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = p.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()));
            let Some(pos) = reader.read_positions() else { continue };
            let base = m.positions.len() as u32;
            m.positions.extend(pos.map(|v| transform(xf, v)));
            let count = m.positions.len() as u32 - base;
            match reader.read_indices() {
                Some(idx) => {
                    let idx: Vec<u32> = idx.into_u32().collect();
                    m.triangles.extend(idx.as_chunks::<3>().0.iter().filter(|t| t.iter().all(|&i| i < count)).map(|t| [base + t[0], base + t[1], base + t[2]]));
                }
                None => m.triangles.extend((0..count / 3).map(|i| [base + i * 3, base + i * 3 + 1, base + i * 3 + 2])),
            }
        }
    };
    let scene = g.default_scene().or_else(|| g.scenes().next());
    match scene {
        Some(s) => {
            // Depth-first with each node's world transform.
            let mut stack: Vec<(gltf::Node, [[f32; 4]; 4])> = s.nodes().map(|n| (n, IDENTITY)).collect();
            let mut seen = 0usize;
            while let Some((node, parent)) = stack.pop() {
                seen += 1;
                if seen > 100_000 {
                    break; // a cycle or a silly file
                }
                let world = mul(&parent, &node.transform().matrix());
                if let Some(mesh) = node.mesh() {
                    add(mesh, &world);
                }
                stack.extend(node.children().map(|c| (c, world)));
            }
        }
        None => {
            for mesh in g.meshes() {
                add(mesh, &IDENTITY);
            }
        }
    }
    Ok(m)
}

const IDENTITY: [[f32; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// Column-major 4×4 product (glTF's layout).
fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut r = [[0.0; 4]; 4];
    for (c, col) in r.iter_mut().enumerate() {
        for (row, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][row] * b[c][k]).sum();
        }
    }
    r
}

fn transform(m: &[[f32; 4]; 4], v: [f32; 3]) -> [f32; 3] {
    let f = |r: usize| m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r];
    [f(0), f(1), f(2)]
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------- FBX

/// Binary FBX: every Geometry's `Vertices` and `PolygonVertexIndex`
/// (ASCII FBX isn't supported).
pub fn fbx_mesh(data: &[u8]) -> Result<Mesh, String> {
    const MAGIC: &[u8] = b"Kaydara FBX Binary  \0";
    if !data.starts_with(MAGIC) || data.len() < 27 {
        return Err("Only binary FBX files can be previewed.".into());
    }
    let version = u32::from_le_bytes(data[23..27].try_into().expect("4 bytes"));
    let wide = version >= 7500;
    let mut m = Mesh::default();
    let mut pos = 27;
    while let Some(next) = fbx_node(data, pos, wide, &mut m, 0)? {
        pos = next;
    }
    Ok(m)
}

#[derive(Default)]
struct Geometry {
    vertices: Option<Vec<f64>>,
    indices: Option<Vec<i32>>,
}

/// Read the node at `pos`; Some(end) to continue, None at the null record.
fn fbx_node(d: &[u8], pos: usize, wide: bool, m: &mut Mesh, depth: u32) -> Result<Option<usize>, String> {
    fbx_node_into(d, pos, wide, m, &mut Geometry::default(), depth)
}

fn fbx_node_into(d: &[u8], pos: usize, wide: bool, m: &mut Mesh, parent: &mut Geometry, depth: u32) -> Result<Option<usize>, String> {
    let bad = || "This FBX file is damaged.".to_string();
    if depth > 64 {
        return Err(bad());
    }
    let word = if wide { 8 } else { 4 };
    let read = |at: usize| -> Option<u64> {
        let b = d.get(at..at + word)?;
        Some(if wide { u64::from_le_bytes(b.try_into().ok()?) } else { u64::from(u32::from_le_bytes(b.try_into().ok()?)) })
    };
    let end = read(pos).ok_or_else(bad)? as usize;
    if end == 0 {
        return Ok(None);
    }
    let prop_len = read(pos + 2 * word).ok_or_else(bad)? as usize;
    let name_len = *d.get(pos + 3 * word).ok_or_else(bad)? as usize;
    let name_at = pos + 3 * word + 1;
    let name = d.get(name_at..name_at + name_len).ok_or_else(bad)?;
    let props = name_at + name_len;
    let children = props + prop_len;
    if end > d.len() || children > end || end <= pos {
        return Err(bad());
    }
    match name {
        b"Vertices" => parent.vertices = fbx_array(d, props, |b| f64::from_le_bytes(b.try_into().expect("8")), |b| f64::from(f32::from_le_bytes(b.try_into().expect("4")))),
        b"PolygonVertexIndex" => parent.indices = fbx_array(d, props, |b| i32::from_le_bytes(b.try_into().expect("4")), |b| i64::from_le_bytes(b.try_into().expect("8")) as i32),
        _ => {}
    }
    // Children, each with its own geometry slot; a Geometry with both
    // arrays becomes triangles.
    let mut geo = Geometry::default();
    let mut at = children;
    while at < end {
        match fbx_node_into(d, at, wide, m, &mut geo, depth + 1)? {
            Some(next) => at = next,
            None => break,
        }
    }
    if let (Some(v), Some(idx)) = (geo.vertices, geo.indices) {
        let base = m.positions.len() as u32;
        m.positions.extend(v.as_chunks::<3>().0.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]));
        let count = m.positions.len() as u32 - base;
        let mut face: Vec<u32> = Vec::new();
        for i in idx {
            // A negative index ends a polygon: its real value is !i.
            let (v, last) = if i < 0 { (!i, true) } else { (i, false) };
            if (v as u32) < count {
                face.push(base + v as u32);
            }
            if last {
                for k in 1..face.len().saturating_sub(1) {
                    m.triangles.push([face[0], face[k], face[k + 1]]);
                }
                face.clear();
            }
        }
    }
    Ok(Some(end))
}

/// The first property at `at`, if it's an array of the wanted kind ('d'/'f'
/// for vertices, 'i'/'l' for indices), inflated when compressed.
fn fbx_array<T>(d: &[u8], at: usize, wide_el: fn(&[u8]) -> T, narrow_el: fn(&[u8]) -> T) -> Option<Vec<T>> {
    let code = *d.get(at)?;
    let (size, conv): (usize, fn(&[u8]) -> T) = match code {
        b'd' | b'l' => (8, if code == b'd' { wide_el } else { narrow_el }),
        b'f' | b'i' => (4, if code == b'f' { narrow_el } else { wide_el }),
        _ => return None,
    };
    let u32_at = |p: usize| d.get(p..p + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4")) as usize);
    let (len, enc, clen) = (u32_at(at + 1)?, u32_at(at + 5)?, u32_at(at + 9)?);
    let raw = d.get(at + 13..at + 13 + clen)?;
    // Never trust the declared length further than the data allows.
    let want = len.checked_mul(size)?;
    if want > 1 << 31 {
        return None;
    }
    let bytes: std::borrow::Cow<[u8]> = if enc == 1 {
        use std::io::Read;
        let mut out = Vec::with_capacity(want.min(1 << 28));
        flate2::read::ZlibDecoder::new(raw).take(want as u64).read_to_end(&mut out).ok()?;
        out.into()
    } else {
        raw.into()
    };
    Some(bytes.chunks_exact(size).take(len).map(conv).collect())
}

// -------------------------------------------------------------- renderer

/// A view of the model: turned `yaw` around the vertical axis and tilted
/// `pitch` toward the viewer, in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for View {
    fn default() -> Self {
        View { yaw: -0.6, pitch: 0.45 }
    }
}

/// The model fitted into a unit sphere at the origin, ready to draw.
pub struct Prepared {
    pos: Vec<[f32; 3]>,
    tris: Vec<[u32; 3]>,
}

impl Prepared {
    pub fn new(mesh: Mesh) -> Self {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in mesh.positions.iter().filter(|p| p.iter().all(|c| c.is_finite())) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
        let r = mesh.positions.iter().filter(|p| p.iter().all(|c| c.is_finite())).map(|p| (0..3).map(|k| (p[k] - c[k]).powi(2)).sum::<f32>()).fold(0.0f32, f32::max).sqrt();
        let s = if r > 0.0 && r.is_finite() { 1.0 / r } else { 1.0 };
        let pos = mesh.positions.iter().map(|p| [(p[0] - c[0]) * s, (p[1] - c[1]) * s, (p[2] - c[2]) * s]).collect();
        Prepared { pos, tris: mesh.triangles }
    }

    pub fn triangles(&self) -> usize {
        self.tris.len()
    }

    /// RGBA pixels (straight alpha, transparent background). `ssaa` 2 draws
    /// at twice the size and averages, for smooth edges.
    pub fn render(&self, view: View, width: u32, height: u32, ssaa: u32) -> Vec<u8> {
        let ssaa = ssaa.clamp(1, 3);
        let (w, h) = ((width * ssaa) as usize, (height * ssaa) as usize);
        let (sy, cy) = view.yaw.sin_cos();
        let (sp, cp) = view.pitch.sin_cos();
        let scale = w.min(h) as f32 * 0.46;
        let (ox, oy) = (w as f32 / 2.0, h as f32 / 2.0);
        // Rotate, then project orthographically: x right, y down, z toward the viewer.
        let screen: Vec<[f32; 3]> = self
            .pos
            .iter()
            .map(|p| {
                let x = p[0] * cy + p[2] * sy;
                let z = -p[0] * sy + p[2] * cy;
                let y2 = p[1] * cp - z * sp;
                let z2 = p[1] * sp + z * cp;
                // z in pixels too, so normals come out right.
                [ox + x * scale, oy - y2 * scale, z2 * scale]
            })
            .collect();
        let mut depth = vec![f32::NEG_INFINITY; w * h];
        let mut shade = vec![0u8; w * h];
        let light = normalize([-0.35, 0.55, 0.75]);
        for t in &self.tris {
            let (Some(a), Some(b), Some(c)) = (screen.get(t[0] as usize), screen.get(t[1] as usize), screen.get(t[2] as usize)) else { continue };
            // Face normal in view space (screen y points down, so flip it).
            let u = [b[0] - a[0], -(b[1] - a[1]), b[2] - a[2]];
            let v = [c[0] - a[0], -(c[1] - a[1]), c[2] - a[2]];
            let n = normalize([u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]);
            // Both sides lit: many models have flipped or open faces.
            let diffuse = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).abs();
            let facing = n[2].abs();
            let lum = (0.28 + 0.62 * diffuse + 0.10 * facing).min(1.0);
            let lum = (lum * 254.0) as u8 + 1; // 0 means empty
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area.abs() < 1e-12 || !area.is_finite() {
                continue;
            }
            let minx = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
            let maxx = (a[0].max(b[0]).max(c[0]).ceil() as isize).min(w as isize - 1);
            let miny = a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize;
            let maxy = (a[1].max(b[1]).max(c[1]).ceil() as isize).min(h as isize - 1);
            if maxx < 0 || maxy < 0 {
                continue;
            }
            for py in miny..=maxy as usize {
                let fy = py as f32 + 0.5;
                for px in minx..=maxx as usize {
                    let fx = px as f32 + 0.5;
                    let w0 = ((b[0] - fx) * (c[1] - fy) - (b[1] - fy) * (c[0] - fx)) / area;
                    let w1 = ((c[0] - fx) * (a[1] - fy) - (c[1] - fy) * (a[0] - fx)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                    let i = py * w + px;
                    if z > depth[i] {
                        depth[i] = z;
                        shade[i] = lum;
                    }
                }
            }
        }
        // Clay colour, then average the supersamples.
        const BASE: [f32; 3] = [205.0, 210.0, 222.0];
        let mut out = vec![0u8; (width * height * 4) as usize];
        let s = ssaa as usize;
        for y in 0..height as usize {
            for x in 0..width as usize {
                let (mut sum, mut covered) = (0.0f32, 0usize);
                for dy in 0..s {
                    for dx in 0..s {
                        let l = shade[(y * s + dy) * w + x * s + dx];
                        if l > 0 {
                            sum += f32::from(l - 1) / 254.0;
                            covered += 1;
                        }
                    }
                }
                if covered == 0 {
                    continue;
                }
                let l = sum / covered as f32;
                let o = (y * width as usize + x) * 4;
                for k in 0..3 {
                    out[o + k] = (BASE[k] * l) as u8;
                }
                out[o + 3] = (255 * covered / (s * s)) as u8;
            }
        }
        out
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 0.0 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 0.0, 1.0] }
}

/// A `px`×`px` thumbnail of the model at `path`.
pub fn thumbnail(path: &Path, px: u32) -> Option<(u32, u32, Vec<u8>)> {
    let p = Prepared::new(load(path).ok()?);
    Some((px, px, p.render(View::default(), px, px, 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE_OBJ: &str = "v -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\n\
        f 1 2 3 4\nf 5/1 6/1 7/1 8/1\nf 1//1 2//1 6//1 5//1\nf -5 -6 -2 -1\nf 1 4 8 5\nf 2 6 7 3\n# comment\nvn 0 0 1\n";

    #[test]
    fn reads_obj() {
        let m = obj_mesh(CUBE_OBJ);
        assert_eq!(m.positions.len(), 8);
        assert_eq!(m.triangles.len(), 12);
        // Relative indices: -5 is vertex 4 (index 3).
        assert_eq!(m.triangles[6], [3, 2, 6]);
    }

    #[test]
    fn reads_stl_both_ways() {
        let ascii = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        let m = stl_mesh(ascii.as_bytes()).unwrap();
        assert_eq!(m.triangles, [[0, 1, 2]]);
        assert_eq!(m.positions[2], [0.0, 0.0, -1.0]); // Z-up turned Y-up
        let mut bin = vec![0u8; 80];
        bin.extend(1u32.to_le_bytes());
        bin.extend([0u8; 12]);
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 0., 1.]] {
            for c in v {
                bin.extend(c.to_le_bytes());
            }
        }
        bin.extend([0u8; 2]);
        let m = stl_mesh(&bin).unwrap();
        assert_eq!(m.positions[2], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn reads_gltf_with_embedded_buffer() {
        use base64::Engine;
        let mut buf = Vec::new();
        for c in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            buf.extend(c.to_le_bytes());
        }
        let uri = format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf));
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"translation":[5,0,0]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
            "bufferViews":[{{"buffer":0,"byteLength":36}}],"buffers":[{{"byteLength":36,"uri":"{uri}"}}]}}"#
        );
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("tri.gltf");
        std::fs::write(&f, json).unwrap();
        let m = load(&f).unwrap();
        assert_eq!(m.triangles, [[0, 1, 2]]);
        assert_eq!(m.positions[1], [6.0, 0.0, 0.0]); // node translation applied
    }

    #[test]
    fn reads_binary_fbx() {
        // FBX 7400: one Objects > Geometry with a quad.
        fn node(name: &str, props: &[u8], children: &[Vec<u8>], at: usize) -> Vec<u8> {
            let body: usize = children.iter().map(|c| c.len()).sum::<usize>() + if children.is_empty() { 0 } else { 13 };
            let end = at + 13 + name.len() + props.len() + body;
            let mut v = Vec::new();
            v.extend((end as u32).to_le_bytes());
            v.extend((u32::from(!props.is_empty())).to_le_bytes());
            v.extend((props.len() as u32).to_le_bytes());
            v.push(name.len() as u8);
            v.extend(name.as_bytes());
            v.extend(props);
            v
        }
        fn array(code: u8, n: usize, data: Vec<u8>) -> Vec<u8> {
            let mut v = vec![code];
            v.extend((n as u32).to_le_bytes());
            v.extend(0u32.to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
            v.extend(data);
            v
        }
        let verts: Vec<u8> = [0f64, 0., 0., 1., 0., 0., 1., 1., 0., 0., 1., 0.].iter().flat_map(|c| c.to_le_bytes()).collect();
        let idx: Vec<u8> = [0i32, 1, 2, !3].iter().flat_map(|c| c.to_le_bytes()).collect();
        let mut file = b"Kaydara FBX Binary  \0\x1a\x00".to_vec();
        file.extend(7400u32.to_le_bytes());
        // Build inside-out so each node knows its end offset.
        let geo_at = file.len() + 13 + "Objects".len();
        let geo_head_len = 13 + "Geometry".len();
        let v_at = geo_at + geo_head_len;
        let v_props = array(b'd', 12, verts);
        let vnode = node("Vertices", &v_props, &[], v_at);
        let i_at = v_at + vnode.len();
        let i_props = array(b'i', 4, idx);
        let inode = node("PolygonVertexIndex", &i_props, &[], i_at);
        let mut geo = node("Geometry", &[], &[vnode.clone(), inode.clone()], geo_at);
        geo.extend(vnode);
        geo.extend(inode);
        geo.extend([0u8; 13]);
        let obj_at = file.len();
        let mut objects = node("Objects", &[], &[geo.clone()], obj_at);
        objects.extend(geo);
        objects.extend([0u8; 13]);
        file.extend(objects);
        file.extend([0u8; 13]);
        let m = fbx_mesh(&file).unwrap();
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.triangles, [[0, 1, 2], [0, 2, 3]]);
        assert!(fbx_mesh(b"; FBX 7.4.0 project file").is_err());
    }

    #[test]
    fn renders_something_centred() {
        let p = Prepared::new(obj_mesh(CUBE_OBJ));
        let px = p.render(View::default(), 64, 64, 2);
        assert_eq!(px.len(), 64 * 64 * 4);
        let alpha = |x: usize, y: usize| px[(y * 64 + x) * 4 + 3];
        assert_eq!(alpha(32, 32), 255, "the middle is covered");
        assert_eq!(alpha(0, 0), 0, "corners stay transparent");
        // Different faces get different shades.
        let mut shades: Vec<u8> = (0..64 * 64).filter(|i| px[i * 4 + 3] == 255).map(|i| px[i * 4]).collect();
        shades.sort();
        shades.dedup();
        assert!(shades.len() >= 3);
    }
}
