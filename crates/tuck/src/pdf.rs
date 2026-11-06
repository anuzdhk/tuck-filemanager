// SPDX-License-Identifier: GPL-3.0-or-later
//! PDF rendering with Poppler. Safe to call from worker threads: each call
//! opens its own document.

use std::path::Path;

use gtk::cairo;

/// One rendered page: premultiplied BGRA (Cairo ARGB32) pixels.
pub struct Page {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub data: Vec<u8>,
}

pub fn page_count(path: &Path) -> Option<i32> {
    let uri = tuck_core::thumbs::uri_for(path);
    poppler::Document::from_file(&uri, None).ok().map(|d| d.n_pages())
}

/// Render page `index` so its longer side is `max_px`.
pub fn render(path: &Path, index: i32, max_px: u32) -> Option<Page> {
    let uri = tuck_core::thumbs::uri_for(path);
    let doc = poppler::Document::from_file(&uri, None).ok()?;
    let page = doc.page(index)?;
    let (w, h) = page.size();
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = max_px as f64 / w.max(h);
    let (pw, ph) = ((w * scale).ceil() as i32, (h * scale).ceil() as i32);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(scale, scale);
        page.render(&cr);
    }
    surface.flush();
    let stride = surface.stride();
    let data = surface.data().ok()?.to_vec();
    Some(Page { width: pw, height: ph, stride, data })
}

/// Page 1 as straight RGBA pixels (for the thumbnail cache).
pub fn first_page_rgba(path: &Path, max_px: u32) -> Option<(u32, u32, Vec<u8>)> {
    let p = render(path, 0, max_px)?;
    let mut out = Vec::with_capacity((p.width * p.height * 4) as usize);
    for row in 0..p.height {
        let line = &p.data[(row * p.stride) as usize..][..(p.width * 4) as usize];
        for px in line.as_chunks::<4>().0 {
            // Cairo ARGB32 in memory (little endian): B, G, R, A, premultiplied.
            let a = px[3] as u32;
            let un = |c: u8| (c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
            out.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), px[3]]);
        }
    }
    Some((p.width as u32, p.height as u32, out))
}

impl Page {
    pub fn texture(self) -> gtk::gdk::Texture {
        let bytes = gtk::glib::Bytes::from_owned(self.data);
        let format = if cfg!(target_endian = "little") {
            gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied
        } else {
            gtk::gdk::MemoryFormat::A8r8g8b8Premultiplied
        };
        gtk::gdk::MemoryTexture::new(self.width, self.height, format, &bytes, self.stride as usize).upcast()
    }
}

use gtk::prelude::Cast;
