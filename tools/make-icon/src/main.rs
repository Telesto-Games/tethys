//! Renders the Tethys icon SVGs into a multi-size .ico (and PNG previews).
//!
//! Usage, from the repo root: `cargo run --manifest-path tools/make-icon/Cargo.toml`

use std::fs::File;
use std::path::Path;

use resvg::{tiny_skia, usvg};

/// Sizes in the .ico. Up to 32px uses the simplified artwork.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const SMALL_MAX: u32 = 32;

fn render(svg: &Path, size: u32) -> tiny_skia::Pixmap {
    let data = std::fs::read(svg).unwrap_or_else(|e| panic!("{}: {e}", svg.display()));
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("valid SVG");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("non-zero size");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
}

fn main() {
    let dir = Path::new("assets/icon");
    let mut icon = ico::IconDir::new(ico::ResourceType::Icon);
    for size in SIZES {
        let source = if size <= SMALL_MAX { "tethys-small.svg" } else { "tethys.svg" };
        let pixmap = render(&dir.join(source), size);
        if size == 256 || size == 32 {
            pixmap
                .save_png(dir.join(format!("tethys-{size}.png")))
                .expect("write png");
        }
        // tiny-skia stores premultiplied RGBA; ICO wants straight alpha.
        let rgba: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect();
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon.add_entry(ico::IconDirEntry::encode(&image).expect("encode"));
    }
    icon.write(File::create(dir.join("tethys.ico")).expect("create ico"))
        .expect("write ico");
    println!("wrote assets/icon/tethys.ico ({} sizes)", SIZES.len());
}
