//! Drawing model shared by the DXF importer, the EZD reader, and the GUI.

/// One continuous laser path, in millimeters. Y grows upward.
#[derive(Clone, Debug)]
pub struct Contour {
    /// When set, the path returns to its first point.
    pub closed: bool,
    /// Absolute coordinates in millimeters.
    pub pts: Vec<[f64; 2]>,
}

/// A named mark object assigned to one pen.
#[derive(Clone, Debug)]
pub struct PathObj {
    /// Name shown in the object list and stored in the `.ezd`.
    pub name: String,
    /// DXF layer name. Empty when the path came from an `.ezd`, which has no layers.
    pub layer: String,
    /// Index into [`Document::pens`].
    pub pen: usize,
    /// Geometry. Empty for notes that carry no path.
    pub contours: Vec<Contour>,
}

/// Laser parameters for one of the 256 EzCad pens.
#[derive(Clone, Debug)]
pub struct Pen {
    /// Pen name stored in the file.
    pub name: String,
    /// Display and mark color, RGB.
    pub color: [u8; 3],
    /// Mark speed in millimeters per second.
    pub speed: f64,
    /// Laser power, 0 to 100 percent.
    pub power: f64,
    /// Pulse frequency in kilohertz.
    pub frequency_khz: f64,
    /// How many times the pen repeats.
    pub passes: i32,
}

impl Pen {
    /// Build a pen with the usual fiber-laser starting values.
    #[must_use]
    pub fn new(index: usize, color: [u8; 3]) -> Self {
        Self {
            name: format!("Pen {index}"),
            color,
            speed: 500.0,
            power: 50.0,
            frequency_khz: 20.0,
            passes: 1,
        }
    }
}

/// Axis-aligned bounds of a drawing, in millimeters.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Left edge.
    pub min_x: f64,
    /// Bottom edge.
    pub min_y: f64,
    /// Right edge.
    pub max_x: f64,
    /// Top edge.
    pub max_y: f64,
}

impl Bounds {
    /// Width in millimeters.
    #[must_use]
    pub fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    /// Height in millimeters.
    #[must_use]
    pub fn height(self) -> f64 {
        self.max_y - self.min_y
    }

    /// Center point.
    #[must_use]
    pub fn center(self) -> [f64; 2] {
        [
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        ]
    }
}

/// A laser job: field, pens, and paths.
#[derive(Clone, Debug)]
pub struct Document {
    /// File name or drawing title, without a directory.
    pub title: String,
    /// Square mark field, in millimeters. EzCad's usual lens is 110.
    pub field_mm: f64,
    /// Always 256 pens. Objects store an index into this list.
    pub pens: Vec<Pen>,
    /// Mark geometry, in draw order.
    pub paths: Vec<PathObj>,
    /// Text strings found in an `.ezd`. Outlines, when present, are paths.
    pub notes: Vec<String>,
}

/// Palette color for pen `index`. The first eight colors repeat.
#[must_use]
pub(crate) fn palette_color(index: usize) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 8] = [
        [0, 0, 0],
        [0, 80, 220],
        [210, 40, 40],
        [20, 150, 60],
        [180, 40, 170],
        [200, 160, 0],
        [0, 160, 170],
        [90, 90, 90],
    ];
    PALETTE[index % PALETTE.len()]
}

impl Document {
    /// Empty job on a 110 mm field with the standard pen colors.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        let pens = (0..256)
            .map(|index| Pen::new(index, palette_color(index)))
            .collect();
        Self {
            title: title.into(),
            field_mm: 110.0,
            pens,
            paths: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Bounds of every path, ignoring empty contours.
    #[must_use]
    pub fn bounds(&self) -> Option<Bounds> {
        let mut bounds: Option<Bounds> = None;
        for path in &self.paths {
            for contour in &path.contours {
                for pt in &contour.pts {
                    bounds = Some(match bounds {
                        None => Bounds {
                            min_x: pt[0],
                            min_y: pt[1],
                            max_x: pt[0],
                            max_y: pt[1],
                        },
                        Some(mut acc) => {
                            acc.min_x = acc.min_x.min(pt[0]);
                            acc.min_y = acc.min_y.min(pt[1]);
                            acc.max_x = acc.max_x.max(pt[0]);
                            acc.max_y = acc.max_y.max(pt[1]);
                            acc
                        }
                    });
                }
            }
        }
        bounds
    }

    /// Move the drawing so its center sits on the field origin.
    pub fn center_on_field(&mut self) {
        let Some(bounds) = self.bounds() else {
            return;
        };
        let [cx, cy] = bounds.center();
        for path in &mut self.paths {
            for contour in &mut path.contours {
                for pt in &mut contour.pts {
                    pt[0] -= cx;
                    pt[1] -= cy;
                }
            }
        }
    }

    /// Number of vertices across every contour.
    #[must_use]
    pub fn point_count(&self) -> usize {
        self.paths
            .iter()
            .flat_map(|path| path.contours.iter())
            .map(|contour| contour.pts.len())
            .sum()
    }
}

/// Triangles that cover a closed contour, as indices into `pts`.
///
/// Convex shapes use a fan. Concave shapes are ear-clipped, so a notch stays empty.
/// An open or degenerate contour returns nothing.
#[must_use]
pub fn fill_triangles(pts: &[[f64; 2]]) -> Vec<[u32; 3]> {
    let mut ring = Vec::new();
    for (index, point) in pts.iter().enumerate() {
        if ring
            .last()
            .copied()
            .is_some_and(|prev| points_touch(pts[prev], *point))
        {
            continue;
        }
        ring.push(index);
    }
    if ring.len() >= 2 && points_touch(pts[ring[0]], pts[*ring.last().unwrap_or(&0)]) {
        ring.pop();
    }
    if ring.len() < 3 {
        return Vec::new();
    }
    let area = ring_area(pts, &ring);
    if area.abs() < 1e-8 {
        return Vec::new();
    }
    let winding = area.signum();
    if ring_is_convex(pts, &ring, winding) {
        return fan(&ring);
    }
    ear_clip(pts, &ring, winding)
}

fn fan(ring: &[usize]) -> Vec<[u32; 3]> {
    (1..ring.len().saturating_sub(1))
        .map(|index| [ring[0] as u32, ring[index] as u32, ring[index + 1] as u32])
        .collect()
}

struct Ear {
    index: usize,
    prev: usize,
    next: usize,
    ear: bool,
}

fn ear_clip(pts: &[[f64; 2]], ring: &[usize], winding: f64) -> Vec<[u32; 3]> {
    let mut verts: Vec<Ear> = ring
        .iter()
        .enumerate()
        .map(|(slot, &index)| Ear {
            index,
            prev: if slot == 0 { ring.len() - 1 } else { slot - 1 },
            next: (slot + 1) % ring.len(),
            ear: false,
        })
        .collect();
    for slot in 0..verts.len() {
        verts[slot].ear = vertex_is_ear(pts, &verts, slot, winding);
    }
    let mut tris = Vec::with_capacity(ring.len().saturating_sub(2));
    let mut count = verts.len();
    let mut cursor = 0;
    while count > 3 {
        let mut slot = cursor;
        let mut found = None;
        for _ in 0..count {
            if verts[slot].ear {
                found = Some(slot);
                break;
            }
            slot = verts[slot].next;
        }
        let Some(slot) = found else {
            break;
        };
        let prev = verts[slot].prev;
        let next = verts[slot].next;
        tris.push([
            verts[prev].index as u32,
            verts[slot].index as u32,
            verts[next].index as u32,
        ]);
        verts[prev].next = next;
        verts[next].prev = prev;
        count -= 1;
        verts[prev].ear = vertex_is_ear(pts, &verts, prev, winding);
        verts[next].ear = vertex_is_ear(pts, &verts, next, winding);
        cursor = prev;
    }
    if count == 3 {
        let slot = cursor;
        let prev = verts[slot].prev;
        let next = verts[slot].next;
        tris.push([
            verts[prev].index as u32,
            verts[slot].index as u32,
            verts[next].index as u32,
        ]);
    }
    tris
}

fn vertex_is_ear(pts: &[[f64; 2]], verts: &[Ear], slot: usize, winding: f64) -> bool {
    let prev = verts[slot].prev;
    let next = verts[slot].next;
    let a = pts[verts[prev].index];
    let b = pts[verts[slot].index];
    let c = pts[verts[next].index];
    // A reflex corner bends back into the shape and is not an ear.
    // A point that sits on a straight edge can be clipped.
    if cross(a, b, c) * winding < -1e-12 {
        return false;
    }
    let mut other = verts[next].next;
    while other != prev {
        let point = pts[verts[other].index];
        if strictly_inside(point, a, b, c) {
            return false;
        }
        other = verts[other].next;
    }
    true
}

fn ring_is_convex(pts: &[[f64; 2]], ring: &[usize], winding: f64) -> bool {
    ring.iter().enumerate().all(|(index, _)| {
        let prev = pts[ring[(index + ring.len() - 1) % ring.len()]];
        let curr = pts[ring[index]];
        let next = pts[ring[(index + 1) % ring.len()]];
        cross(prev, curr, next) * winding > 1e-12
    })
}

fn ring_area(pts: &[[f64; 2]], ring: &[usize]) -> f64 {
    let mut sum = 0.0;
    for index in 0..ring.len() {
        let current = pts[ring[index]];
        let next = pts[ring[(index + 1) % ring.len()]];
        sum += current[0] * next[1] - next[0] * current[1];
    }
    sum * 0.5
}

fn strictly_inside(point: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let ab = cross(a, b, point);
    let bc = cross(b, c, point);
    let ca = cross(c, a, point);
    (ab > 1e-12 && bc > 1e-12 && ca > 1e-12) || (ab < -1e-12 && bc < -1e-12 && ca < -1e-12)
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn points_touch(a: [f64; 2], b: [f64; 2]) -> bool {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy < 1e-12
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_of(pts: &[[f64; 2]], tris: &[[u32; 3]]) -> f64 {
        tris.iter()
            .map(|tri| {
                cross(
                    pts[tri[0] as usize],
                    pts[tri[1] as usize],
                    pts[tri[2] as usize],
                )
                .abs()
                    * 0.5
            })
            .sum()
    }

    #[test]
    fn a_square_fills_with_two_triangles() {
        let pts = [[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]];
        let tris = fill_triangles(&pts);
        assert_eq!(tris.len(), 2);
        assert!((area_of(&pts, &tris) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_notch_stays_outside_the_fill() {
        let pts = [
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [3.0, 2.0],
            [3.0, 3.0],
            [0.0, 3.0],
        ];
        let tris = fill_triangles(&pts);
        assert_eq!(tris.len(), 6);
        assert!((area_of(&pts, &tris) - 7.0).abs() < 1e-9);
        let notch = [2.0, 1.5];
        assert!(
            tris.iter().all(|tri| {
                !strictly_inside(
                    notch,
                    pts[tri[0] as usize],
                    pts[tri[1] as usize],
                    pts[tri[2] as usize],
                )
            }),
            "the notch was filled"
        );
    }
}
