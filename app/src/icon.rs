//! The app icon, drawn in code: a violet-to-pink rounded square with a white sparkle. Plain Rust
//! with no dependencies: the build script draws the exe's icon with it too.

/// Straight (not premultiplied) RGBA, rows top to bottom.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded square, signed distance with a one-pixel soft edge
            let (r, m) = (s * 0.24, s * 0.03);
            let half = s / 2.0 - m - r;
            let (dx, dy) = ((fx - s / 2.0).abs() - half, (fy - s / 2.0).abs() - half);
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            let t = (fx + fy) / (2.0 * s);
            let base = [124.0 + (255.0 - 124.0) * t, 77.0 + (92.0 - 77.0) * t, 255.0 + (170.0 - 255.0) * t];
            // four-point sparkle: |x|^0.5 + |y|^0.5 < 1
            let (sx, sy) = ((fx - s * 0.5) / (s * 0.34), (fy - s * 0.5) / (s * 0.34));
            let star = sx.abs().sqrt() + sy.abs().sqrt();
            let white = ((1.0 - star) * s * 0.12).clamp(0.0, 1.0);
            let i = ((y * size + x) * 4) as usize;
            for c in 0..3 {
                out[i + c] = (base[c] + (255.0 - base[c]) * white).round() as u8;
            }
            out[i + 3] = (cover * 255.0).round() as u8;
        }
    }
    out
}
