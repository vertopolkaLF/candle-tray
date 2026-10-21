//! Regenerate the Windows application icon from `assets/app-icon.svg` using cached resvg.
use std::{env, fs, io, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(env::args_os().nth(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "pass the repository root")
        })?);
    let svg = fs::read_to_string(root.join("assets/app-icon.svg"))?;
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())?;
    let mut images = Vec::new();
    for size in [16_u32, 20, 24, 32, 40, 48, 64, 256] {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or("allocate pixmap")?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(size as f32 / 256.0, size as f32 / 256.0),
            &mut pixmap.as_mut(),
        );
        let png = pixmap.encode_png()?;
        if size == 256 {
            fs::write(root.join("assets/app-icon.png"), &png)?;
        }
        images.push((size, png));
    }
    // ICO supports PNG frames (Windows Vista+), including their full alpha channel.
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + images.len() as u32 * 16;
    for (size, png) in &images {
        let dimension = if *size == 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[dimension, dimension, 0, 0]);
        ico.extend_from_slice(&1_u16.to_le_bytes());
        ico.extend_from_slice(&32_u16.to_le_bytes());
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        ico.extend_from_slice(&png);
    }
    fs::write(root.join("assets/app-icon.ico"), ico)?;
    Ok(())
}
