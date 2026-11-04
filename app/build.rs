// The exe's icon and version information, for Explorer, the taskbar and the file's properties.

#[path = "src/icon.rs"]
mod icon;

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("app.ico");
    std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])).expect("app.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("icon path"))
        .set("ProductName", "Ableton Skin Lab")
        .set("FileDescription", "Ableton Skin Lab")
        .set("CompanyName", "blueberry")
        .set("LegalCopyright", "© 2026 blueberry")
        .set("OriginalFilename", "AbletonSkinLab.exe")
        .set("InternalName", "AbletonSkinLab");
    res.compile().expect("resources");
}

/// An .ico with one 32-bit BMP image per size.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp(s)).collect();
    let mut f = vec![0, 0, 1, 0];
    f.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let side = if s >= 256 { 0 } else { s as u8 };
        f.extend_from_slice(&[side, side, 0, 0]);
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(&(img.len() as u32).to_le_bytes());
        f.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in &images {
        f.extend_from_slice(img);
    }
    f
}

/// A BITMAPINFOHEADER (twice the height: the colours, then the mask), BGRA rows bottom up and an
/// all-clear AND mask: the alpha channel decides.
fn bmp(s: u32) -> Vec<u8> {
    let rgba = icon::rgba(s);
    let mask_row = s.div_ceil(32) * 4;
    let mut b = Vec::new();
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(s as i32).to_le_bytes());
    b.extend_from_slice(&(2 * s as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&(s * s * 4 + mask_row * s).to_le_bytes());
    b.extend_from_slice(&[0u8; 16]);
    for y in (0..s).rev() {
        for x in 0..s {
            let i = ((y * s + x) * 4) as usize;
            b.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    b.resize(b.len() + (mask_row * s) as usize, 0);
    b
}
