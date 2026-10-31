//! Tray icon drawn in code, so the binary stays self-contained: a fish inside a
//! note head. The ring is accent-blue while playing and muted grey when stopped.
//!
//! The design is drawn on a 100x100 grid and then zoomed so it fills the canvas
//! like the other tray icons (claude-desktop fills about 91% of its 64 px pixmap).

use ksni::Icon;

const SIZES: [i32; 3] = [22, 32, 48];
const SUBSAMPLES: u32 = 4;

/// Zoom of the design about its centre. 1.36 makes the note head span about 90% of the canvas.
const ZOOM: f32 = 1.36;
/// Fish size relative to the note head, so it reads at tray size.
const FISH: f32 = 1.15;

type Rgb = (u8, u8, u8);

struct Palette {
    ring_playing: Rgb,
    ring_stopped: Rgb,
    fish: Rgb,
    cut: Rgb,
}

const DARK_PANEL: Palette = Palette {
    ring_playing: (0x42, 0xa5, 0xf5),
    ring_stopped: (0x90, 0xa4, 0xaf),
    fish: (0xec, 0xef, 0xf1),
    cut: (0x20, 0x23, 0x26),
};

const LIGHT_PANEL: Palette = Palette {
    ring_playing: (0x1e, 0x88, 0xe5),
    ring_stopped: (0x78, 0x90, 0x9c),
    fish: (0x26, 0x32, 0x38),
    cut: (0xef, 0xf0, 0xf1),
};

/// Icon set for the given state and panel colour scheme.
pub fn render(playing: bool, dark_panel: bool) -> Vec<Icon> {
    let palette = if dark_panel { &DARK_PANEL } else { &LIGHT_PANEL };
    let ring = if playing { palette.ring_playing } else { palette.ring_stopped };
    SIZES.iter().map(|&size| draw(size, ring, palette)).collect()
}

fn draw(size: i32, ring: Rgb, palette: &Palette) -> Icon {
    let n = size as usize;
    let mut data = Vec::with_capacity(n * n * 4);
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0u32;
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let px = (x as f32 + (sx as f32 + 0.5) / SUBSAMPLES as f32) / size as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SUBSAMPLES as f32) / size as f32;
                    let (dx, dy) = to_design(px * 100.0, py * 100.0);
                    if let Some((cr, cg, cb)) = sample(dx, dy, ring, palette) {
                        covered += 1;
                        r += cr as u32;
                        g += cg as u32;
                        b += cb as u32;
                    }
                }
            }
            if covered == 0 {
                data.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let alpha = (covered * 255 / (SUBSAMPLES * SUBSAMPLES)) as u8;
                data.extend_from_slice(&[
                    alpha,
                    (r / covered) as u8,
                    (g / covered) as u8,
                    (b / covered) as u8,
                ]);
            }
        }
    }
    Icon { width: size, height: size, data }
}

/// Maps canvas coordinates (0..100) to design coordinates, zoomed about the design centre (50, 55).
fn to_design(cx: f32, cy: f32) -> (f32, f32) {
    ((cx - 50.0) / ZOOM + 50.0, (cy - 50.0) / ZOOM + 55.0)
}

/// Colour at a point in design space, or None if outside the icon.
fn sample(x: f32, y: f32, ring: Rgb, palette: &Palette) -> Option<Rgb> {
    if in_eye(x, y) {
        return Some(palette.cut);
    }
    if in_fish(x, y) {
        return Some(palette.fish);
    }
    if in_ring(x, y) {
        return Some(ring);
    }
    None
}

/// Note head: a ring, ellipse rx 30 / ry 22, rotated -20 degrees, stroke 7.
fn in_ring(x: f32, y: f32) -> bool {
    let (u, v) = to_note_local(x, y);
    let outer = (u / 33.5).powi(2) + (v / 25.5).powi(2) <= 1.0;
    let inner = (u / 26.5).powi(2) + (v / 18.5).powi(2) >= 1.0;
    outer && inner
}

/// Maps a design-space point into the note head's unrotated frame (centre 50,55).
fn to_note_local(x: f32, y: f32) -> (f32, f32) {
    let (dx, dy) = (x - 50.0, y - 55.0);
    let (s, c) = 20f32.to_radians().sin_cos();
    (c * dx - s * dy, s * dx + c * dy)
}

/// Fish: ellipse body plus a triangular tail on the left, scaled by FISH about (50, 55).
fn in_fish(x: f32, y: f32) -> bool {
    let rx = 15.0 * FISH;
    let ry = 9.0 * FISH;
    let body = ((x - 50.0) / rx).powi(2) + ((y - 55.0) / ry).powi(2) <= 1.0;
    let tail_x = 50.0 - 0.85 * rx;
    let tip_x = 50.0 - 1.5 * rx;
    let tail_half = 0.95 * ry;
    body || in_triangle((x, y), (tail_x, 55.0), (tip_x, 55.0 - tail_half), (tip_x, 55.0 + tail_half))
}

/// Eye: a small circle in panel colour, placed in the fish's forward half.
fn in_eye(x: f32, y: f32) -> bool {
    let (ex, ey, er) = (50.0 + 7.5 * FISH, 55.0 - 1.8 * FISH, 1.44 * FISH);
    (x - ex).powi(2) + (y - ey).powi(2) <= er.powi(2)
}

fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
