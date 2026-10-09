//! ASCII DXF reader and writer.
//!
//! Imports markable outlines: lines, polylines, circles, arcs, ellipses, and
//! splines, including geometry pulled in by `INSERT`. Solid hatches are skipped
//! because their boundaries repeat the splines already in the drawing.
//!
//! The writer emits AutoCAD Release 12 (`AC1009`) ASCII: one `POLYLINE` /
//! `VERTEX` / `SEQEND` per contour. Release 12 is the interchange DXF that
//! eDrawings and EzCad open without handles or subclass markers. A file that
//! claims `AC1021` and then omits those records is not a DXF those programs
//! can load. Color is the ACI index (group 62). Group 420 is not a Release 12
//! code, and a strict reader discards the drawing when it sees one. A camada
//! used by one pen keeps its name. A camada used by several pens is split, one
//! layer per pen, because EzCad assigns a single color to a layer. A path that
//! came from an `.ezd` has no layer, so it is written on a layer named after
//! its pen.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::geom::{palette_color, Contour, Document, PathObj};
use crate::{Error, Result};

/// A stored polyline stays within this distance of the true DXF curve.
const CHORD_TOLERANCE_MM: f64 = 0.01;

/// Eight splits is 256 segments on one spline span.
const MAX_FLATTEN_DEPTH: u32 = 8;

/// A huge radius cannot ask for more samples than this.
const MAX_ARC_STEPS: usize = 2048;

/// Read an ASCII DXF and center it on the 110 mm field.
///
/// # Errors
///
/// Returns an error for binary DXF, unreadable files, or drawings with no
/// supported geometry.
pub fn read_dxf(path: &Path) -> Result<Document> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    if bytes.windows(3).any(|window| window == b"\0\0\0") && !bytes.starts_with(b"  0") {
        return Err(Error::Format(
            "this DXF is binary. In your CAD program, save it as ASCII DXF and open that."
                .to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let pairs = parse_pairs(&text);
    let (layers, blocks, model) = split_sections(&pairs);
    let mut doc = Document::new(file_title(path));
    let mut index = 0_usize;
    for entity in &model {
        if entity.ty == "INSERT" {
            expand_insert(&mut doc, &blocks, &layers, entity, &mut index);
        } else {
            push_entity(&mut doc, &layers, entity, &mut index);
        }
    }
    if doc.paths.is_empty() {
        return Err(Error::Format(
            "the DXF has no lines, polylines, or splines to mark".to_owned(),
        ));
    }
    doc.center_on_field();
    Ok(doc)
}

/// Write `doc` as an AutoCAD Release 12 ASCII DXF.
///
/// Coordinates are millimeters. Release 12 has no `$INSUNITS` variable, which
/// is how laser and CAM programs exchange DXF. Each contour becomes a
/// `POLYLINE` with `VERTEX` records and a `SEQEND`. The layer name is the
/// path's camada. When the path has none, the layer is the pen name. The
/// entity and the layer both carry the nearest ACI color (group 62).
///
/// # Errors
///
/// Returns an error when the destination cannot be created.
pub fn write_dxf(path: &Path, doc: &Document) -> Result<()> {
    let bytes = encode_dxf(doc);
    let mut file = fs::File::create(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    file.write_all(&bytes).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}

struct Entity {
    ty: String,
    layer: String,
    fields: Vec<(i32, String)>,
    /// Points taken from `VERTEX` records that follow a `POLYLINE`.
    /// The polyline entity's own groups 10/20 are its elevation, not vertices.
    vertices: Vec<PolylineVertex>,
}

struct PolylineVertex {
    x: f64,
    y: f64,
    bulge: f64,
}

struct Block {
    entities: Vec<Entity>,
}

struct LayerPaint {
    aci: i32,
    true_color: Option<[u8; 3]>,
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("drawing")
        .to_owned()
}

fn parse_pairs(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines().map(str::trim);
    let mut pairs = Vec::new();
    while let Some(code) = lines.next() {
        let Some(value) = lines.next() else {
            break;
        };
        if code.is_empty() {
            continue;
        }
        pairs.push((code.to_owned(), value.to_owned()));
    }
    pairs
}

fn split_sections(
    pairs: &[(String, String)],
) -> (
    HashMap<String, LayerPaint>,
    HashMap<String, Block>,
    Vec<Entity>,
) {
    let mut section = String::new();
    let mut layers = HashMap::new();
    let mut blocks: HashMap<String, Block> = HashMap::new();
    let mut model = Vec::new();
    let mut block_name: Option<String> = None;
    let mut block_ents: Vec<Entity> = Vec::new();
    let mut current: Option<Entity> = None;

    let flush = |current: &mut Option<Entity>,
                 block_name: &Option<String>,
                 block_ents: &mut Vec<Entity>,
                 model: &mut Vec<Entity>| {
        let Some(entity) = current.take() else {
            return;
        };
        if !is_geometry(&entity.ty) && entity.ty != "INSERT" {
            return;
        }
        if block_name.is_some() {
            block_ents.push(entity);
        } else {
            model.push(entity);
        }
    };

    let mut index = 0;
    while index < pairs.len() {
        let code = pairs[index].0.clone();
        let value = pairs[index].1.clone();
        if code == "0" {
            let continues_polyline = current
                .as_ref()
                .is_some_and(|entity| entity.ty == "POLYLINE")
                && (value == "VERTEX" || value == "SEQEND");
            if !continues_polyline {
                flush(&mut current, &block_name, &mut block_ents, &mut model);
            }
            match value.as_str() {
                "VERTEX" if continues_polyline => {
                    if let Some(entity) = current.as_mut() {
                        absorb_vertex(entity, pairs, &mut index);
                    }
                    continue;
                }
                "SEQEND" if continues_polyline => {
                    skip_until_next_entity(pairs, &mut index);
                    continue;
                }
                "SECTION" => {
                    if let Some(("2", name)) = pairs.get(index + 1).map(|(c, v)| (c.as_str(), v)) {
                        section.clone_from(name);
                    }
                }
                "ENDSEC" => {
                    section.clear();
                    if let Some(name) = block_name.take() {
                        blocks.insert(
                            name,
                            Block {
                                entities: std::mem::take(&mut block_ents),
                            },
                        );
                    }
                }
                "BLOCK" if section == "BLOCKS" => {
                    if let Some(name) = block_name.take() {
                        blocks.insert(
                            name,
                            Block {
                                entities: std::mem::take(&mut block_ents),
                            },
                        );
                    }
                    current = Some(read_record(pairs, &mut index));
                    if let Some(entity) = current.take() {
                        block_name = field_str(&entity, 2);
                    }
                    continue;
                }
                "ENDBLK" => {
                    if let Some(name) = block_name.take() {
                        blocks.insert(
                            name,
                            Block {
                                entities: std::mem::take(&mut block_ents),
                            },
                        );
                    }
                }
                "LAYER" if section == "TABLES" => {
                    let entity = read_record(pairs, &mut index);
                    if let Some(name) = field_str(&entity, 2) {
                        layers.insert(
                            name,
                            LayerPaint {
                                aci: field_int(&entity, 62).unwrap_or(7),
                                true_color: true_color_of(&entity),
                            },
                        );
                    }
                    continue;
                }
                ty if section == "ENTITIES" || block_name.is_some() => {
                    if is_geometry(ty) || ty == "INSERT" {
                        current = Some(Entity {
                            ty: ty.to_owned(),
                            layer: String::new(),
                            fields: Vec::new(),
                            vertices: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        } else if let Some(entity) = current.as_mut() {
            if let Ok(group) = code.parse::<i32>() {
                if group == 8 {
                    entity.layer.clone_from(&value);
                }
                entity.fields.push((group, value.clone()));
            }
        }
        index += 1;
    }
    flush(&mut current, &block_name, &mut block_ents, &mut model);
    (layers, blocks, model)
}

fn read_record(pairs: &[(String, String)], index: &mut usize) -> Entity {
    let ty = pairs[*index].1.clone();
    *index += 1;
    let mut entity = Entity {
        ty,
        layer: String::new(),
        fields: Vec::new(),
        vertices: Vec::new(),
    };
    while *index < pairs.len() && pairs[*index].0 != "0" {
        if let Ok(group) = pairs[*index].0.parse::<i32>() {
            let value = pairs[*index].1.clone();
            if group == 8 {
                entity.layer.clone_from(&value);
            }
            entity.fields.push((group, value));
        }
        *index += 1;
    }
    entity
}

/// `index` points at the `0` / `VERTEX` pair. Leaves it on the next `0` pair.
fn absorb_vertex(entity: &mut Entity, pairs: &[(String, String)], index: &mut usize) {
    *index += 1;
    let mut x = None;
    let mut y = None;
    let mut bulge = 0.0;
    while *index < pairs.len() && pairs[*index].0 != "0" {
        if let Ok(group) = pairs[*index].0.parse::<i32>() {
            match group {
                10 => x = pairs[*index].1.parse().ok(),
                20 => y = pairs[*index].1.parse().ok(),
                42 => bulge = pairs[*index].1.parse().unwrap_or(0.0),
                _ => {}
            }
        }
        *index += 1;
    }
    if let (Some(x), Some(y)) = (x, y) {
        entity.vertices.push(PolylineVertex { x, y, bulge });
    }
}

/// `index` points at a `0` pair. Leaves it on the next `0` pair.
fn skip_until_next_entity(pairs: &[(String, String)], index: &mut usize) {
    *index += 1;
    while *index < pairs.len() && pairs[*index].0 != "0" {
        *index += 1;
    }
}

fn is_geometry(ty: &str) -> bool {
    matches!(
        ty,
        "LINE" | "LWPOLYLINE" | "POLYLINE" | "CIRCLE" | "ARC" | "ELLIPSE" | "SPLINE" | "POINT"
    )
}

fn field_str(entity: &Entity, group: i32) -> Option<String> {
    entity
        .fields
        .iter()
        .find(|(code, _)| *code == group)
        .map(|(_, value)| value.clone())
}

fn field_f64(entity: &Entity, group: i32) -> Option<f64> {
    field_str(entity, group).and_then(|value| value.parse().ok())
}

fn field_int(entity: &Entity, group: i32) -> Option<i32> {
    field_str(entity, group).and_then(|value| value.parse().ok())
}

fn values(entity: &Entity, group: i32) -> Vec<f64> {
    entity
        .fields
        .iter()
        .filter(|(code, _)| *code == group)
        .filter_map(|(_, value)| value.parse().ok())
        .collect()
}

fn assign_pen(
    doc: &mut Document,
    layers: &HashMap<String, LayerPaint>,
    entity: &Entity,
    layer: &str,
) -> usize {
    if let Some(rgb) = true_color_of(entity) {
        return pen_for_rgb(doc, rgb);
    }
    let raw = field_int(entity, 62).unwrap_or(256);
    if raw != 256 {
        return aci_to_pen(raw);
    }
    match layers.get(layer) {
        Some(paint) => match paint.true_color {
            Some(rgb) => pen_for_rgb(doc, rgb),
            None => aci_to_pen(paint.aci),
        },
        None => aci_to_pen(7),
    }
}

fn aci_to_pen(aci: i32) -> usize {
    match aci {
        1 => 2,
        2 => 5,
        3 => 3,
        4 => 6,
        5 => 1,
        6 => 4,
        _ => 0,
    }
}

fn true_color_of(entity: &Entity) -> Option<[u8; 3]> {
    let value = field_int(entity, 420)?;
    let value = u32::try_from(value).ok()?;
    if value > 0x00FF_FFFF {
        return None;
    }
    Some([
        u8::try_from((value >> 16) & 0xFF).unwrap_or(0),
        u8::try_from((value >> 8) & 0xFF).unwrap_or(0),
        u8::try_from(value & 0xFF).unwrap_or(0),
    ])
}

fn pen_for_rgb(doc: &mut Document, rgb: [u8; 3]) -> usize {
    if let Some(index) = doc.pens.iter().position(|pen| pen.color == rgb) {
        return index;
    }
    if let Some(index) =
        (8..doc.pens.len()).find(|index| doc.pens[*index].color == palette_color(*index))
    {
        doc.pens[index].color = rgb;
        return index;
    }
    0
}

fn effective_layer(entity: &Entity) -> String {
    let name = entity.layer.trim();
    if name.is_empty() {
        "0".to_owned()
    } else {
        name.to_owned()
    }
}

/// Block geometry on layer 0 takes the INSERT's layer, matching a CAD display.
fn placed_layer(child: &Entity, insert: &Entity) -> String {
    let child_layer = child.layer.trim();
    if child_layer.is_empty() || child_layer == "0" {
        effective_layer(insert)
    } else {
        child_layer.to_owned()
    }
}

fn push_entity(
    doc: &mut Document,
    layers: &HashMap<String, LayerPaint>,
    entity: &Entity,
    index: &mut usize,
) {
    let Some(contours) = contours_of(entity) else {
        return;
    };
    if contours.iter().all(|contour| contour.pts.len() < 2) {
        return;
    }
    *index += 1;
    let kind = match entity.ty.as_str() {
        "LWPOLYLINE" | "POLYLINE" => "Polyline",
        "SPLINE" => "Spline",
        "LINE" => "Line",
        "CIRCLE" => "Circle",
        "ARC" => "Arc",
        "ELLIPSE" => "Ellipse",
        _ => "Path",
    };
    let layer = effective_layer(entity);
    let pen = assign_pen(doc, layers, entity, &layer);
    doc.paths.push(PathObj {
        name: format!("{kind} {index}"),
        pen,
        layer,
        contours,
    });
}

fn expand_insert(
    doc: &mut Document,
    blocks: &HashMap<String, Block>,
    layers: &HashMap<String, LayerPaint>,
    entity: &Entity,
    index: &mut usize,
) {
    let Some(name) = field_str(entity, 2) else {
        return;
    };
    let Some(block) = blocks.get(&name) else {
        return;
    };
    let origin_x = field_f64(entity, 10).unwrap_or(0.0);
    let origin_y = field_f64(entity, 20).unwrap_or(0.0);
    let scale_x = field_f64(entity, 41).unwrap_or(1.0);
    let scale_y = field_f64(entity, 42).unwrap_or(1.0);
    let rotation = field_f64(entity, 50).unwrap_or(0.0).to_radians();
    let (sin, cos) = rotation.sin_cos();
    for child in &block.entities {
        let Some(mut contours) = contours_of(child) else {
            continue;
        };
        for contour in &mut contours {
            for pt in &mut contour.pts {
                let x = pt[0] * scale_x;
                let y = pt[1] * scale_y;
                pt[0] = origin_x + x * cos - y * sin;
                pt[1] = origin_y + x * sin + y * cos;
            }
        }
        if contours.iter().all(|contour| contour.pts.len() < 2) {
            continue;
        }
        *index += 1;
        let layer = placed_layer(child, entity);
        let pen = assign_pen(doc, layers, child, &layer);
        doc.paths.push(PathObj {
            name: format!("{name} {index}"),
            pen,
            layer,
            contours,
        });
    }
}

fn contours_of(entity: &Entity) -> Option<Vec<Contour>> {
    let contours = match entity.ty.as_str() {
        "LINE" => line(entity),
        "POINT" => Vec::new(),
        "LWPOLYLINE" => lwpolyline(entity),
        "POLYLINE" => polyline(entity),
        "CIRCLE" => circle(entity),
        "ARC" => arc(entity),
        "ELLIPSE" => ellipse(entity),
        "SPLINE" => spline(entity),
        _ => Vec::new(),
    };
    if contours.is_empty() {
        None
    } else {
        Some(contours)
    }
}

fn line(entity: &Entity) -> Vec<Contour> {
    let Some(x1) = field_f64(entity, 10) else {
        return Vec::new();
    };
    let Some(y1) = field_f64(entity, 20) else {
        return Vec::new();
    };
    let Some(x2) = field_f64(entity, 11) else {
        return Vec::new();
    };
    let Some(y2) = field_f64(entity, 21) else {
        return Vec::new();
    };
    vec![Contour {
        closed: false,
        pts: vec![[x1, y1], [x2, y2]],
    }]
}

fn lwpolyline(entity: &Entity) -> Vec<Contour> {
    let xs = values(entity, 10);
    let ys = values(entity, 20);
    let bulges = values(entity, 42);
    let count = xs.len().min(ys.len());
    if count < 2 {
        return Vec::new();
    }
    let closed = field_int(entity, 70).unwrap_or(0) & 1 == 1;
    let mut pts = Vec::new();
    let segments = if closed { count } else { count - 1 };
    for index in 0..segments {
        let next = (index + 1) % count;
        let start = [xs[index], ys[index]];
        let end = [xs[next], ys[next]];
        let bulge = bulges.get(index).copied().unwrap_or(0.0);
        if index == 0 {
            pts.push(start);
        }
        append_bulge(&mut pts, start, end, bulge);
    }
    vec![Contour { closed, pts }]
}

fn polyline(entity: &Entity) -> Vec<Contour> {
    let flags = field_int(entity, 70).unwrap_or(0);
    // Bit 4 is a polygon mesh and bit 6 is a polyface mesh. Their VERTEX
    // records are a grid, not a mark path.
    if flags & 16 != 0 || flags & 64 != 0 || entity.vertices.len() < 2 {
        return Vec::new();
    }
    let closed = flags & 1 == 1;
    let count = entity.vertices.len();
    let mut pts = Vec::new();
    let segments = if closed { count } else { count - 1 };
    for index in 0..segments {
        let next = (index + 1) % count;
        let start = [entity.vertices[index].x, entity.vertices[index].y];
        let end = [entity.vertices[next].x, entity.vertices[next].y];
        if index == 0 {
            pts.push(start);
        }
        append_bulge(&mut pts, start, end, entity.vertices[index].bulge);
    }
    vec![Contour { closed, pts }]
}

fn append_bulge(pts: &mut Vec<[f64; 2]>, start: [f64; 2], end: [f64; 2], bulge: f64) {
    if bulge.abs() < 1e-8 {
        pts.push(end);
        return;
    }
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let chord = (dx * dx + dy * dy).sqrt();
    if chord < 1e-9 {
        pts.push(end);
        return;
    }
    let theta = 4.0 * bulge.atan();
    let radius = chord / (2.0 * (theta / 2.0).sin());
    let mid_x = (start[0] + end[0]) / 2.0;
    let mid_y = (start[1] + end[1]) / 2.0;
    let offset = (radius * radius - (chord / 2.0).powi(2)).max(0.0).sqrt();
    let sign = if bulge >= 0.0 { 1.0 } else { -1.0 };
    let center_x = mid_x + sign * (-dy / chord) * offset;
    let center_y = mid_y + sign * (dx / chord) * offset;
    let start_angle = (start[1] - center_y).atan2(start[0] - center_x);
    let steps = steps_for_sweep(radius.abs(), theta.abs());
    for step in 1..=steps {
        let angle = start_angle + theta * (step as f64 / steps as f64);
        pts.push([
            center_x + radius * angle.cos(),
            center_y + radius * angle.sin(),
        ]);
    }
}

fn circle(entity: &Entity) -> Vec<Contour> {
    let Some(cx) = field_f64(entity, 10) else {
        return Vec::new();
    };
    let Some(cy) = field_f64(entity, 20) else {
        return Vec::new();
    };
    let Some(radius) = field_f64(entity, 40) else {
        return Vec::new();
    };
    Some(arc_points(
        cx,
        cy,
        radius,
        radius,
        0.0,
        std::f64::consts::TAU,
        true,
    ))
    .into_iter()
    .collect()
}

fn arc(entity: &Entity) -> Vec<Contour> {
    let Some(cx) = field_f64(entity, 10) else {
        return Vec::new();
    };
    let Some(cy) = field_f64(entity, 20) else {
        return Vec::new();
    };
    let Some(radius) = field_f64(entity, 40) else {
        return Vec::new();
    };
    let start = field_f64(entity, 50).unwrap_or(0.0).to_radians();
    let mut end = field_f64(entity, 51).unwrap_or(0.0).to_radians();
    if end <= start {
        end += std::f64::consts::TAU;
    }
    vec![arc_points(cx, cy, radius, radius, start, end, false)]
}

fn ellipse(entity: &Entity) -> Vec<Contour> {
    let Some(cx) = field_f64(entity, 10) else {
        return Vec::new();
    };
    let Some(cy) = field_f64(entity, 20) else {
        return Vec::new();
    };
    let Some(mx) = field_f64(entity, 11) else {
        return Vec::new();
    };
    let Some(my) = field_f64(entity, 21) else {
        return Vec::new();
    };
    let ratio = field_f64(entity, 40).unwrap_or(1.0);
    let start = field_f64(entity, 41).unwrap_or(0.0);
    let mut end = field_f64(entity, 42).unwrap_or(std::f64::consts::TAU);
    if (end - start).abs() < 1e-6 {
        end = start + std::f64::consts::TAU;
    }
    let major = (mx * mx + my * my).sqrt();
    let angle = my.atan2(mx);
    let (sin, cos) = angle.sin_cos();
    let minor = major * ratio;
    let closed = (end - start).abs() >= std::f64::consts::TAU - 1e-3;
    let steps = steps_for_sweep(major.max(minor.abs()), end - start);
    let mut pts = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let t = start + (end - start) * (step as f64 / steps as f64);
        let x = major * t.cos();
        let y = minor * t.sin();
        pts.push([cx + x * cos - y * sin, cy + x * sin + y * cos]);
    }
    vec![Contour { closed, pts }]
}

fn arc_points(cx: f64, cy: f64, rx: f64, ry: f64, start: f64, end: f64, closed: bool) -> Contour {
    let sweep = end - start;
    let steps = steps_for_sweep(rx.abs().max(ry.abs()), sweep);
    let mut pts = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let t = start + sweep * (step as f64 / steps as f64);
        pts.push([cx + rx * t.cos(), cy + ry * t.sin()]);
    }
    Contour { closed, pts }
}

fn spline(entity: &Entity) -> Vec<Contour> {
    let degree = field_int(entity, 71).unwrap_or(3).max(1) as usize;
    let flags = field_int(entity, 70).unwrap_or(0);
    let knots = values(entity, 40);
    let weights = values(entity, 41);
    let xs = values(entity, 10);
    let ys = values(entity, 20);
    let count = xs.len().min(ys.len());
    if count < 2 {
        let fit_x = values(entity, 11);
        let fit_y = values(entity, 21);
        let fit = fit_x.len().min(fit_y.len());
        if fit < 2 {
            return Vec::new();
        }
        return vec![Contour {
            closed: flags & 1 == 1,
            pts: (0..fit).map(|index| [fit_x[index], fit_y[index]]).collect(),
        }];
    }
    let controls: Vec<[f64; 2]> = (0..count).map(|index| [xs[index], ys[index]]).collect();
    if knots.len() < degree + count + 1 || degree == 1 {
        return vec![Contour {
            closed: flags & 1 == 1,
            pts: controls,
        }];
    }
    let mut pts = Vec::new();
    let last_knot = knots[knots.len() - degree - 1];
    let mut span = degree;
    while span + 1 < knots.len() && span < count {
        let left = knots[span];
        let right = knots[span + 1];
        if right > left {
            flatten_span(
                &mut pts,
                &SplineSpan {
                    index: span,
                    degree,
                    knots: &knots,
                    controls: &controls,
                    weights: &weights,
                },
                left,
                right,
                0,
            );
        }
        span += 1;
        if knots[span] > last_knot && span > degree {
            break;
        }
    }
    pts.push(de_boor(
        (count - 1).max(degree),
        degree,
        last_knot,
        &knots,
        &controls,
        &weights,
    ));
    dedup(&mut pts);
    let closed =
        flags & 1 == 1 || pts.len() >= 2 && dist(pts[0], *pts.last().unwrap_or(&pts[0])) < 0.05;
    vec![Contour { closed, pts }]
}

/// Step count whose circular chord stays within [`CHORD_TOLERANCE_MM`].
///
/// `sagitta = radius * (1 - cos(step / 2))`, so
/// `step = 2 * acos(1 - tolerance / radius)`.
fn steps_for_sweep(radius: f64, sweep: f64) -> usize {
    let sweep = sweep.abs();
    if sweep < 1e-12 {
        return 1;
    }
    let radius = radius.abs();
    if radius < 1e-12 {
        return 1;
    }
    let ratio = (CHORD_TOLERANCE_MM / radius).min(2.0);
    let step = 2.0 * (1.0 - ratio).clamp(-1.0, 1.0).acos();
    let step = if step < 1e-6 {
        std::f64::consts::TAU
    } else {
        step
    };
    let steps = (sweep / step).ceil();
    let steps = if steps.is_finite() { steps } else { 1.0 };
    let mut steps = steps.clamp(1.0, MAX_ARC_STEPS as f64) as usize;
    // A full turn with one step is a point. Four sides still sit inside the tolerance
    // once the radius itself is smaller than that tolerance.
    if sweep >= std::f64::consts::TAU - 1e-3 {
        steps = steps.max(4);
    }
    steps
}

struct SplineSpan<'a> {
    index: usize,
    degree: usize,
    knots: &'a [f64],
    controls: &'a [[f64; 2]],
    weights: &'a [f64],
}

/// Push the start of each flat piece. The caller pushes the span's final point once.
fn flatten_span(
    pts: &mut Vec<[f64; 2]>,
    curve: &SplineSpan<'_>,
    left: f64,
    right: f64,
    depth: u32,
) {
    let start = curve.point(left);
    if depth >= MAX_FLATTEN_DEPTH {
        pts.push(start);
        return;
    }
    let end = curve.point(right);
    let mid_u = (left + right) * 0.5;
    let mid = curve.point(mid_u);
    if chord_gap(mid, start, end) <= CHORD_TOLERANCE_MM {
        pts.push(start);
        return;
    }
    flatten_span(pts, curve, left, mid_u, depth + 1);
    flatten_span(pts, curve, mid_u, right, depth + 1);
}

impl SplineSpan<'_> {
    fn point(&self, u: f64) -> [f64; 2] {
        de_boor(
            self.index,
            self.degree,
            u,
            self.knots,
            self.controls,
            self.weights,
        )
    }
}

fn chord_gap(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-18 {
        return dist(point, start);
    }
    let t = (((point[0] - start[0]) * dx + (point[1] - start[1]) * dy) / len2).clamp(0.0, 1.0);
    let x = start[0] + t * dx - point[0];
    let y = start[1] + t * dy - point[1];
    (x * x + y * y).sqrt()
}

fn de_boor(
    span: usize,
    degree: usize,
    u: f64,
    knots: &[f64],
    controls: &[[f64; 2]],
    weights: &[f64],
) -> [f64; 2] {
    let mut points = Vec::with_capacity(degree + 1);
    let mut ws = Vec::with_capacity(degree + 1);
    for offset in 0..=degree {
        let index = span - degree + offset;
        let index = index.min(controls.len().saturating_sub(1));
        let weight = weights.get(index).copied().unwrap_or(1.0);
        points.push([controls[index][0] * weight, controls[index][1] * weight]);
        ws.push(weight);
    }
    for round in 1..=degree {
        for index in (round..=degree).rev() {
            let knot_index = span - degree + index;
            let left = knots.get(knot_index).copied().unwrap_or(u);
            let right = knots
                .get(knot_index + degree - round + 1)
                .copied()
                .unwrap_or(left);
            let denom = right - left;
            let alpha = if denom.abs() < 1e-12 {
                0.0
            } else {
                (u - left) / denom
            };
            points[index][0] = (1.0 - alpha) * points[index - 1][0] + alpha * points[index][0];
            points[index][1] = (1.0 - alpha) * points[index - 1][1] + alpha * points[index][1];
            ws[index] = (1.0 - alpha) * ws[index - 1] + alpha * ws[index];
        }
    }
    let weight = if ws[degree].abs() < 1e-12 {
        1.0
    } else {
        ws[degree]
    };
    [points[degree][0] / weight, points[degree][1] / weight]
}

fn dedup(pts: &mut Vec<[f64; 2]>) {
    pts.dedup_by(|a, b| dist(*a, *b) < 0.002);
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

fn encode_dxf(doc: &Document) -> Vec<u8> {
    let split = camadas_with_several_pens(doc);
    let layers = export_layers(doc, &split);
    let (ext_min, ext_max) = drawing_extents(doc);
    let mut out = String::new();
    // AutoCAD Release 12. Claiming AC1021 without handles, subclass markers,
    // CLASSES, and OBJECTS is the file eDrawings rejects. Group 420 is not a
    // Release 12 code; TrueView discards the drawing on an unknown group.
    write_header(&mut out, ext_min, ext_max);
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "TABLES");
    write_ltype_table(&mut out);
    write_layer_table(&mut out, doc, &layers);
    write_style_table(&mut out);
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "BLOCKS");
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "ENTITIES");
    for path in &doc.paths {
        let layer_name = export_layer_name(path, doc, &split);
        let aci = nearest_aci(pen_rgb(doc, path.pen)).to_string();
        for contour in &path.contours {
            let vertices = polyline_vertices(contour);
            if vertices.len() < 2 {
                continue;
            }
            pair(&mut out, 0, "POLYLINE");
            pair(&mut out, 8, &layer_name);
            pair(&mut out, 6, "CONTINUOUS");
            pair(&mut out, 62, &aci);
            pair(&mut out, 66, "1");
            pair(&mut out, 70, if contour.closed { "1" } else { "0" });
            // Groups 10/20/30 on POLYLINE are the elevation, always 0 here.
            // The mark points live on the VERTEX records.
            pair(&mut out, 10, "0.0");
            pair(&mut out, 20, "0.0");
            pair(&mut out, 30, "0.0");
            for point in vertices {
                pair(&mut out, 0, "VERTEX");
                pair(&mut out, 8, &layer_name);
                pair(&mut out, 6, "CONTINUOUS");
                pair(&mut out, 10, &format_mm(point[0]));
                pair(&mut out, 20, &format_mm(point[1]));
                pair(&mut out, 30, "0.0");
            }
            pair(&mut out, 0, "SEQEND");
            pair(&mut out, 8, &layer_name);
        }
    }
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "EOF");
    // The header names the code page ANSI_1252. ASCII is unchanged. A character
    // outside Latin-1 cannot be stored in that code page, so it becomes '_'.
    latin1_bytes(&out)
}

fn latin1_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len());
    for ch in text.chars() {
        match u8::try_from(u32::from(ch)) {
            Ok(byte) => bytes.push(byte),
            Err(_) => bytes.push(b'_'),
        }
    }
    bytes
}

fn drawing_extents(doc: &Document) -> ([f64; 2], [f64; 2]) {
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    let mut any = false;
    for path in &doc.paths {
        for contour in &path.contours {
            for point in polyline_vertices(contour) {
                any = true;
                min[0] = min[0].min(point[0]);
                min[1] = min[1].min(point[1]);
                max[0] = max[0].max(point[0]);
                max[1] = max[1].max(point[1]);
            }
        }
    }
    if any {
        (min, max)
    } else {
        ([0.0, 0.0], [0.0, 0.0])
    }
}

fn write_header(out: &mut String, min: [f64; 2], max: [f64; 2]) {
    pair(out, 0, "SECTION");
    pair(out, 2, "HEADER");
    pair(out, 9, "$ACADVER");
    pair(out, 1, "AC1009");
    pair(out, 9, "$DWGCODEPAGE");
    pair(out, 3, "ANSI_1252");
    pair(out, 9, "$INSBASE");
    pair(out, 10, "0.0");
    pair(out, 20, "0.0");
    pair(out, 30, "0.0");
    pair(out, 9, "$EXTMIN");
    pair(out, 10, &format_mm(min[0]));
    pair(out, 20, &format_mm(min[1]));
    pair(out, 30, "0.0");
    pair(out, 9, "$EXTMAX");
    pair(out, 10, &format_mm(max[0]));
    pair(out, 20, &format_mm(max[1]));
    pair(out, 30, "0.0");
    pair(out, 9, "$LIMMIN");
    pair(out, 10, &format_mm(min[0]));
    pair(out, 20, &format_mm(min[1]));
    pair(out, 9, "$LIMMAX");
    pair(out, 10, &format_mm(max[0]));
    pair(out, 20, &format_mm(max[1]));
    pair(out, 9, "$CLAYER");
    pair(out, 8, "0");
    pair(out, 9, "$CELTYPE");
    pair(out, 6, "BYLAYER");
    pair(out, 9, "$CECOLOR");
    pair(out, 62, "256");
    pair(out, 9, "$TEXTSTYLE");
    pair(out, 7, "STANDARD");
    pair(out, 9, "$LTSCALE");
    pair(out, 40, "1.0");
    // AutoCAD 2000 added these. Release 12 readers skip a header variable
    // they do not know. EzCad looks up both names: 4 is millimeters, 1 is metric.
    pair(out, 9, "$INSUNITS");
    pair(out, 70, "4");
    pair(out, 9, "$MEASUREMENT");
    pair(out, 70, "1");
    pair(out, 0, "ENDSEC");
}

struct ExportLayer {
    name: String,
    pen: usize,
}

fn export_layers(doc: &Document, split: &HashSet<String>) -> Vec<ExportLayer> {
    let mut order = vec!["0".to_owned()];
    let mut pens: HashMap<String, usize> = HashMap::new();
    for path in &doc.paths {
        if !has_exportable_contour(path) {
            continue;
        }
        let name = export_layer_name(path, doc, split);
        if !order.iter().any(|existing| existing == &name) {
            order.push(name.clone());
        }
        pens.entry(name).or_insert(path.pen);
    }
    order
        .into_iter()
        .map(|name| ExportLayer {
            pen: pens.get(&name).copied().unwrap_or(0),
            name,
        })
        .collect()
}

/// Camadas whose exported contours use more than one pen. Those are split so
/// EzCad, which keeps one color per layer, does not paint them all blue.
fn camadas_with_several_pens(doc: &Document) -> HashSet<String> {
    let mut first_pen: HashMap<&str, usize> = HashMap::new();
    let mut split = HashSet::new();
    for path in &doc.paths {
        let name = path.layer.trim();
        if name.is_empty() || !has_exportable_contour(path) {
            continue;
        }
        match first_pen.get(name).copied() {
            Some(pen) if pen != path.pen => {
                split.insert(name.to_owned());
            }
            Some(_) => {}
            None => {
                first_pen.insert(name, path.pen);
            }
        }
    }
    split
}

fn has_exportable_contour(path: &PathObj) -> bool {
    path.contours.iter().any(|contour| contour.pts.len() >= 2)
}

fn export_layer_name(path: &PathObj, doc: &Document, split: &HashSet<String>) -> String {
    let camada = path.layer.trim();
    let raw = if camada.is_empty() {
        pen_label(doc, path.pen)
    } else if split.contains(camada) {
        format!("{camada} {}", pen_label(doc, path.pen))
    } else {
        camada.to_owned()
    };
    sanitize_layer(&raw)
}

fn pen_label(doc: &Document, index: usize) -> String {
    if let Some(pen) = doc.pens.get(index) {
        let name = pen.name.trim();
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    format!("Pen {index}")
}

fn sanitize_layer(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_control()
            || matches!(
                ch,
                '<' | '>' | '/' | '\\' | '"' | ':' | ';' | '?' | '*' | '|' | ',' | '=' | '`'
            )
        {
            out.push('_');
        } else {
            out.push(ch);
        }
        if out.chars().count() == 255 {
            break;
        }
    }
    if out.is_empty() {
        "0".to_owned()
    } else {
        out
    }
}

fn pen_rgb(doc: &Document, pen: usize) -> [u8; 3] {
    doc.pens.get(pen).map(|pen| pen.color).unwrap_or([0, 0, 0])
}

fn nearest_aci(rgb: [u8; 3]) -> i32 {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    if max <= 16 || min >= 230 {
        return 7;
    }
    if u16::from(max - min) <= 24 {
        return 8;
    }
    const CHOICES: [([u8; 3], i32); 6] = [
        ([255, 0, 0], 1),
        ([255, 255, 0], 2),
        ([0, 255, 0], 3),
        ([0, 255, 255], 4),
        ([0, 0, 255], 5),
        ([255, 0, 255], 6),
    ];
    let mut best = 7;
    let mut best_dist = i32::MAX;
    for (color, aci) in CHOICES {
        let dist = channel_dist(rgb, color);
        if dist < best_dist {
            best_dist = dist;
            best = aci;
        }
    }
    best
}

fn channel_dist(left: [u8; 3], right: [u8; 3]) -> i32 {
    let mut total = 0_i32;
    for index in 0..3 {
        let delta = i32::from(left[index]) - i32::from(right[index]);
        total += delta * delta;
    }
    total
}

fn polyline_vertices(contour: &Contour) -> Vec<[f64; 2]> {
    let mut pts = contour.pts.clone();
    if contour.closed && pts.len() >= 2 && dist(pts[0], *pts.last().unwrap_or(&pts[0])) < 1e-6 {
        pts.pop();
    }
    pts
}

fn write_ltype_table(out: &mut String) {
    pair(out, 0, "TABLE");
    pair(out, 2, "LTYPE");
    pair(out, 70, "3");
    // An entity that omits group 6 is ByLayer. The table has to define that
    // name, plus ByBlock, or the loader fails while resolving the linetype.
    write_ltype(out, "BYBLOCK", "BYBLOCK");
    write_ltype(out, "BYLAYER", "BYLAYER");
    write_ltype(out, "CONTINUOUS", "Solid line");
    pair(out, 0, "ENDTAB");
}

fn write_ltype(out: &mut String, name: &str, description: &str) {
    pair(out, 0, "LTYPE");
    pair(out, 2, name);
    pair(out, 70, "0");
    pair(out, 3, description);
    pair(out, 72, "65");
    pair(out, 73, "0");
    pair(out, 40, "0.0");
}

fn write_layer_table(out: &mut String, doc: &Document, layers: &[ExportLayer]) {
    pair(out, 0, "TABLE");
    pair(out, 2, "LAYER");
    pair(out, 70, &layers.len().to_string());
    for layer in layers {
        let rgb = pen_rgb(doc, layer.pen);
        pair(out, 0, "LAYER");
        pair(out, 2, &layer.name);
        pair(out, 70, "0");
        pair(out, 62, &nearest_aci(rgb).to_string());
        pair(out, 6, "CONTINUOUS");
    }
    pair(out, 0, "ENDTAB");
}

fn write_style_table(out: &mut String) {
    pair(out, 0, "TABLE");
    pair(out, 2, "STYLE");
    pair(out, 70, "1");
    pair(out, 0, "STYLE");
    pair(out, 2, "STANDARD");
    pair(out, 70, "0");
    pair(out, 40, "0.0");
    pair(out, 41, "1.0");
    pair(out, 50, "0.0");
    pair(out, 71, "0");
    pair(out, 42, "2.5");
    pair(out, 3, "txt");
    // Group 4 is the bigfont file. AutoCAD writes an empty value here.
    pair(out, 4, "");
    pair(out, 0, "ENDTAB");
}

fn pair(out: &mut String, code: i32, value: &str) {
    out.push_str(&format!("{code:3}\r\n{value}\r\n"));
}

fn format_mm(value: f64) -> String {
    if value.abs() < 5e-7 {
        "0.0".to_owned()
    } else {
        format!("{value:.6}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxf_save_keeps_the_camada_and_the_pen_color() {
        let mut doc = Document::new("square");
        doc.paths.push(PathObj {
            name: "Box".into(),
            layer: "Camada 1".into(),
            pen: 2,
            contours: vec![Contour {
                closed: true,
                pts: vec![[-10.0, -5.0], [10.0, -5.0], [10.0, 5.0], [-10.0, 5.0]],
            }],
        });
        let path = std::env::temp_dir().join("ezd-studio-square.dxf");
        write_dxf(&path, &doc).expect("write");
        let text = std::fs::read_to_string(&path).expect("read text");
        assert_release_12(&text);
        assert!(text.contains("Camada 1"));
        assert!(text.contains(" 62\r\n1\r\n"));
        assert_polylines_are_complete(&text);
        let loaded = read_dxf(&path).expect("read");
        let bounds = loaded.bounds().expect("bounds");
        assert!((bounds.min_x - -10.0).abs() < 1e-4);
        assert!((bounds.max_x - 10.0).abs() < 1e-4);
        assert!((bounds.min_y - -5.0).abs() < 1e-4);
        assert!((bounds.max_y - 5.0).abs() < 1e-4);
        assert_eq!(loaded.paths.len(), 1);
        assert_eq!(loaded.paths[0].layer, "Camada 1");
        assert_eq!(loaded.pens[loaded.paths[0].pen].color, doc.pens[2].color);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_different_pen_on_the_same_camada_keeps_its_own_color() {
        let mut doc = Document::new("two");
        doc.paths.push(square_path("A", "Camada 1", 1, -10.0));
        doc.paths.push(square_path("B", "Camada 1", 2, 10.0));
        let path = std::env::temp_dir().join("ezd-studio-two-colors.dxf");
        write_dxf(&path, &doc).expect("write");
        let text = std::fs::read_to_string(&path).expect("text");
        assert!(text.contains("Camada 1 Pen 1"));
        assert!(text.contains("Camada 1 Pen 2"));
        assert!(text.contains(" 62\r\n5\r\n"));
        assert!(text.contains(" 62\r\n1\r\n"));
        let loaded = read_dxf(&path).expect("read");
        assert_eq!(loaded.paths.len(), 2);
        assert_eq!(loaded.paths[0].layer, "Camada 1 Pen 1");
        assert_eq!(loaded.paths[1].layer, "Camada 1 Pen 2");
        assert_eq!(loaded.pens[loaded.paths[0].pen].color, doc.pens[1].color);
        assert_eq!(loaded.pens[loaded.paths[1].pen].color, doc.pens[2].color);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_path_without_a_camada_uses_the_pen_name() {
        let mut doc = Document::new("pen-layer");
        doc.pens[5].name = "Corte".into();
        doc.pens[5].color = [255, 128, 0];
        doc.paths.push(square_path("A", "", 5, 0.0));
        let path = std::env::temp_dir().join("ezd-studio-pen-layer.dxf");
        write_dxf(&path, &doc).expect("write");
        let text = std::fs::read_to_string(&path).expect("text");
        assert!(text.contains("Corte"));
        // Release 12 stores the nearest ACI, not the exact RGB. Orange is yellow.
        assert!(text.contains(" 62\r\n2\r\n"));
        let loaded = read_dxf(&path).expect("read");
        assert_eq!(loaded.paths[0].layer, "Corte");
        assert_eq!(loaded.paths[0].pen, 5);
        assert_eq!(loaded.pens[loaded.paths[0].pen].color, palette_color(5));
        let _ = std::fs::remove_file(path);
    }

    fn assert_release_12(text: &str) {
        assert!(text.starts_with("  0\r\nSECTION\r\n  2\r\nHEADER\r\n"));
        assert!(text.contains("$ACADVER\r\n  1\r\nAC1009\r\n"));
        assert!(text.contains("$DWGCODEPAGE\r\n  3\r\nANSI_1252\r\n"));
        assert!(text.contains("$INSUNITS\r\n 70\r\n4\r\n"));
        assert!(text.contains("$MEASUREMENT\r\n 70\r\n1\r\n"));
        assert!(text.contains("  2\r\nTABLES\r\n"));
        assert!(text.contains("  2\r\nBLOCKS\r\n"));
        assert!(text.contains("  2\r\nENTITIES\r\n"));
        assert!(text.contains("BYBLOCK"));
        assert!(text.contains("BYLAYER"));
        assert!(text.contains("CONTINUOUS"));
        assert!(text.contains("STANDARD"));
        assert!(text.ends_with("  0\r\nEOF\r\n"));
        assert!(!text.contains("LWPOLYLINE"));
        assert!(!text.contains("AC1021"));
        assert!(!text.contains("UTF-8"));
        assert!(!text.contains("\r\n100\r\n"));
        assert!(!text.contains("\r\n330\r\n"));
        assert!(!text.contains("\r\n420\r\n"));
        assert!(!text.contains("\r\n 90\r\n"));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    }

    fn assert_polylines_are_complete(text: &str) {
        let pairs = parse_pairs(text);
        let mut index = 0;
        let mut in_entities = false;
        let mut polylines = 0;
        while index < pairs.len() {
            if pairs[index].0 == "0" && pairs[index].1 == "SECTION" {
                in_entities = pairs
                    .get(index + 1)
                    .is_some_and(|pair| pair.0 == "2" && pair.1 == "ENTITIES");
            }
            if pairs[index].0 == "0" && pairs[index].1 == "ENDSEC" {
                in_entities = false;
            }
            if in_entities && pairs[index].0 == "0" && pairs[index].1 == "POLYLINE" {
                polylines += 1;
                let mut saw_vertices_follow = false;
                let mut saw_color = false;
                index += 1;
                while index < pairs.len() && pairs[index].0 != "0" {
                    if pairs[index].0 == "66" && pairs[index].1 == "1" {
                        saw_vertices_follow = true;
                    }
                    if pairs[index].0 == "62" {
                        saw_color = true;
                    }
                    index += 1;
                }
                assert!(saw_vertices_follow, "POLYLINE missing group 66");
                assert!(saw_color, "POLYLINE missing group 62");
                let mut vertices = 0;
                while index < pairs.len() && pairs[index].1 == "VERTEX" {
                    vertices += 1;
                    index += 1;
                    let mut saw_x = false;
                    let mut saw_y = false;
                    while index < pairs.len() && pairs[index].0 != "0" {
                        saw_x |= pairs[index].0 == "10";
                        saw_y |= pairs[index].0 == "20";
                        index += 1;
                    }
                    assert!(saw_x && saw_y, "VERTEX missing 10/20");
                }
                assert!(vertices >= 2, "POLYLINE has {vertices} vertices");
                assert_eq!(pairs[index].1, "SEQEND");
                continue;
            }
            index += 1;
        }
        assert!(polylines >= 1);
    }

    #[test]
    fn layer_names_drop_characters_dxf_rejects() {
        assert_eq!(sanitize_layer("a/b:c"), "a_b_c");
    }

    #[test]
    fn a_bent_spline_stays_within_the_chord_tolerance() {
        let path = entities_dxf(
            "ezd-studio-bent-spline.dxf",
            &spline_entity(&[[0.0, 0.0], [0.0, 40.0], [40.0, 40.0], [40.0, 0.0]]),
        );
        let doc = read_dxf(&path).expect("read");
        let pts = &doc.paths[0].contours[0].pts;
        assert!(pts.len() > 2, "points {}", pts.len());
        let knots = [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let controls = [[0.0, 0.0], [0.0, 40.0], [40.0, 40.0], [40.0, 0.0]];
        let shift = [pts[0][0] - controls[0][0], pts[0][1] - controls[0][1]];
        let mut worst = 0.0_f64;
        for step in 0..=64 {
            let sample = de_boor(3, 3, step as f64 / 64.0, &knots, &controls, &[]);
            let sample = [sample[0] + shift[0], sample[1] + shift[1]];
            let gap = pts
                .windows(2)
                .map(|window| chord_gap(sample, window[0], window[1]))
                .fold(f64::MAX, f64::min);
            worst = worst.max(gap);
        }
        assert!(worst <= CHORD_TOLERANCE_MM + 1e-6, "chord error {worst}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_straight_spline_stays_two_points() {
        let path = entities_dxf(
            "ezd-studio-straight-spline.dxf",
            &spline_entity(&[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]]),
        );
        let doc = read_dxf(&path).expect("read");
        assert_eq!(doc.paths[0].contours[0].pts.len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_circle_stays_within_the_chord_tolerance() {
        let path = entities_dxf(
            "ezd-studio-circle.dxf",
            "  0\nCIRCLE\n  8\n0\n 10\n0.0\n 20\n0.0\n 40\n25.0\n",
        );
        let doc = read_dxf(&path).expect("read");
        let pts = &doc.paths[0].contours[0].pts;
        assert!(pts.len() > 4, "points {}", pts.len());
        let closing = dist(pts[0], *pts.last().expect("circle")) < 1e-6;
        let body = if closing { &pts[..pts.len() - 1] } else { pts };
        let (mut cx, mut cy) = (0.0, 0.0);
        for point in body {
            cx += point[0];
            cy += point[1];
        }
        cx /= body.len() as f64;
        cy /= body.len() as f64;
        for point in body {
            let radius = dist(*point, [cx, cy]);
            assert!((radius - 25.0).abs() < 1e-4, "radius {radius}");
        }
        for window in pts.windows(2) {
            if dist(window[0], window[1]) < 1e-9 {
                continue;
            }
            let start = (window[0][1] - cy).atan2(window[0][0] - cx);
            let end = (window[1][1] - cy).atan2(window[1][0] - cx);
            let mut sweep = end - start;
            if sweep < 0.0 {
                sweep += std::f64::consts::TAU;
            }
            let mid_angle = start + sweep * 0.5;
            let mid = [cx + 25.0 * mid_angle.cos(), cy + 25.0 * mid_angle.sin()];
            let gap = chord_gap(mid, window[0], window[1]);
            assert!(gap <= CHORD_TOLERANCE_MM + 1e-6, "sagitta {gap}");
        }
        let _ = std::fs::remove_file(path);
    }

    fn entities_dxf(name: &str, entities: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        let text = format!("  0\nSECTION\n  2\nENTITIES\n{entities}  0\nENDSEC\n  0\nEOF\n");
        std::fs::write(&path, text).expect("write fixture");
        path
    }

    fn spline_entity(controls: &[[f64; 2]]) -> String {
        let mut out = String::from("  0\nSPLINE\n  8\n0\n 70\n8\n 71\n3\n");
        for point in controls {
            out.push_str(&format!(" 10\n{}\n 20\n{}\n", point[0], point[1]));
        }
        for _ in 0..4 {
            out.push_str(" 40\n0.0\n");
        }
        for _ in 0..4 {
            out.push_str(" 40\n1.0\n");
        }
        out
    }

    fn square_path(name: &str, layer: &str, pen: usize, x: f64) -> PathObj {
        PathObj {
            name: name.into(),
            layer: layer.into(),
            pen,
            contours: vec![Contour {
                closed: true,
                pts: vec![[x, 0.0], [x + 2.0, 0.0], [x + 2.0, 2.0], [x, 2.0]],
            }],
        }
    }
}
