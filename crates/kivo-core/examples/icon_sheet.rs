// Writes every generated folder/document icon as SVG into a folder (for review).
use kivo_core::icons::{Doc, Folder, doc_svg, folder_svg};
use kivo_core::theme::Rgb;
fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output dir"));
    std::fs::create_dir_all(&out).unwrap();
    let accents = [("blue", "#3584e4"), ("purple", "#9141ac"), ("yellow", "#c88800")];
    for (an, a) in accents {
        for f in [Folder::Plain, Folder::Home, Folder::Documents, Folder::Downloads, Folder::Pictures, Folder::Music, Folder::Videos, Folder::Desktop] {
            std::fs::write(out.join(format!("a-{an}-{}.svg", f.id())), folder_svg(f, Rgb::parse(a).unwrap())).unwrap();
        }
    }
    for (d, b) in [(Doc::Pdf, "PDF"), (Doc::Image, "JPG"), (Doc::Video, "MP4"), (Doc::Audio, "MP3"), (Doc::Archive, "ZIP"), (Doc::Code, "RS"), (Doc::Text, "TXT"), (Doc::Document, "DOCX"), (Doc::Spreadsheet, "XLSX"), (Doc::Presentation, "PPTX"), (Doc::App, "DEB"), (Doc::Generic, "")] {
        std::fs::write(out.join(format!("b-{}.svg", d.id())), doc_svg(d, b)).unwrap();
    }
}
