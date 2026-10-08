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
