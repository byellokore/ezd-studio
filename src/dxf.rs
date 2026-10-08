//! ASCII DXF reader and writer.
//!
//! Imports markable outlines: lines, polylines, circles, arcs, ellipses, and
//! splines, including geometry pulled in by `INSERT`. Solid hatches are skipped
//! because their boundaries repeat the splines already in the drawing.
//!
//! The writer emits one LWPOLYLINE per contour. A path keeps its DXF layer.
//! A path that came from an `.ezd` has no layer, so it is written on a layer
//! named after its pen. The layer color is that pen's RGB.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::geom::{palette_color, Contour, Document, PathObj};
use crate::{Error, Result};

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

/// Write `doc` as an ASCII DXF, in millimeters.
///
/// Each contour becomes an `LWPOLYLINE`. The layer name is the path's camada.
/// When the path has none, the layer is the pen name. The layer's color is the
/// pen color used by most paths on that layer. A path whose pen color differs
/// carries its own color on the entity.
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
            flush(&mut current, &block_name, &mut block_ents, &mut model);
            match value.as_str() {
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
    if let Some(index) = (8..doc.pens.len())
        .find(|index| doc.pens[*index].color == palette_color(*index))
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
    let steps = (theta.abs() / std::f64::consts::FRAC_PI_8).ceil().clamp(4.0, 64.0) as usize;
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
    Some(arc_points(cx, cy, radius, radius, 0.0, std::f64::consts::TAU, true))
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
    let steps = 72;
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
    let steps = (sweep.abs() / std::f64::consts::FRAC_PI_8)
        .ceil()
        .clamp(12.0, 96.0) as usize;
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
            let samples = 4;
            for step in 0..samples {
                let u = left + (right - left) * (step as f64 / samples as f64);
                pts.push(de_boor(span, degree, u, &knots, &controls, &weights));
            }
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
    let closed = flags & 1 == 1
        || pts.len() >= 2 && dist(pts[0], *pts.last().unwrap_or(&pts[0])) < 0.05;
    vec![Contour { closed, pts }]
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
    let layers = export_layers(doc);
    let mut out = String::new();
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "HEADER");
    pair(&mut out, 9, "$ACADVER");
    pair(&mut out, 1, "AC1021");
    pair(&mut out, 9, "$INSUNITS");
    pair(&mut out, 70, "4");
    pair(&mut out, 9, "$DWGCODEPAGE");
    pair(&mut out, 3, "UTF-8");
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "TABLES");
    write_ltype_table(&mut out);
    write_layer_table(&mut out, doc, &layers);
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "SECTION");
    pair(&mut out, 2, "ENTITIES");
    for path in &doc.paths {
        let layer_name = export_layer_name(path, doc);
        let layer_pen = layers
            .iter()
            .find(|layer| layer.name == layer_name)
            .map(|layer| layer.pen)
            .unwrap_or(path.pen);
        let layer_rgb = pen_rgb(doc, layer_pen);
        for contour in &path.contours {
            let vertices = polyline_vertices(contour);
            if vertices.len() < 2 {
                continue;
            }
            pair(&mut out, 0, "LWPOLYLINE");
            pair(&mut out, 8, &layer_name);
            let rgb = pen_rgb(doc, path.pen);
            if rgb != layer_rgb {
                pair(&mut out, 62, &nearest_aci(rgb).to_string());
                pair(&mut out, 420, &true_color_code(rgb).to_string());
            }
            pair(&mut out, 90, &vertices.len().to_string());
            pair(&mut out, 70, if contour.closed { "1" } else { "0" });
            for point in vertices {
                pair(&mut out, 10, &format_mm(point[0]));
                pair(&mut out, 20, &format_mm(point[1]));
            }
        }
    }
    pair(&mut out, 0, "ENDSEC");
    pair(&mut out, 0, "EOF");
    out.into_bytes()
}

struct ExportLayer {
    name: String,
    pen: usize,
}

fn export_layers(doc: &Document) -> Vec<ExportLayer> {
    let mut order = vec!["0".to_owned()];
    let mut counts: HashMap<String, HashMap<usize, usize>> = HashMap::new();
    for path in &doc.paths {
        let name = export_layer_name(path, doc);
        if !order.iter().any(|existing| existing == &name) {
            order.push(name.clone());
        }
        let contours = path
            .contours
            .iter()
            .filter(|contour| contour.pts.len() >= 2)
            .count();
        if contours == 0 {
            continue;
        }
        *counts
            .entry(name)
            .or_default()
            .entry(path.pen)
            .or_default() += contours;
    }
    order
        .into_iter()
        .map(|name| ExportLayer {
            pen: counts
                .get(&name)
                .map(majority_pen)
                .unwrap_or(0),
            name,
        })
        .collect()
}

fn majority_pen(counts: &HashMap<usize, usize>) -> usize {
    counts
        .iter()
        .max_by_key(|(pen, count)| (*count, std::cmp::Reverse(*pen)))
        .map(|(pen, _)| *pen)
        .unwrap_or(0)
}

fn export_layer_name(path: &PathObj, doc: &Document) -> String {
    let raw = if path.layer.trim().is_empty() {
        doc.pens
            .get(path.pen)
            .map(|pen| pen.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("Pen {}", path.pen))
    } else {
        path.layer.clone()
    };
    sanitize_layer(&raw)
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

fn true_color_code(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

fn polyline_vertices(contour: &Contour) -> Vec<[f64; 2]> {
    let mut pts = contour.pts.clone();
    if contour.closed
        && pts.len() >= 2
        && dist(pts[0], *pts.last().unwrap_or(&pts[0])) < 1e-6
    {
        pts.pop();
    }
    pts
}

fn write_ltype_table(out: &mut String) {
    pair(out, 0, "TABLE");
    pair(out, 2, "LTYPE");
    pair(out, 70, "1");
    pair(out, 0, "LTYPE");
    pair(out, 2, "CONTINUOUS");
    pair(out, 70, "0");
    pair(out, 3, "Solid line");
    pair(out, 72, "65");
    pair(out, 73, "0");
    pair(out, 40, "0.0");
    pair(out, 0, "ENDTAB");
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
        pair(out, 420, &true_color_code(rgb).to_string());
        pair(out, 6, "CONTINUOUS");
    }
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
        assert!(text.contains("Camada 1"));
        assert!(text.contains(&true_color_code(doc.pens[2].color).to_string()));
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
        let loaded = read_dxf(&path).expect("read");
        assert_eq!(loaded.paths.len(), 2);
        assert!(loaded.paths.iter().all(|path| path.layer == "Camada 1"));
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
        assert!(text.contains(&true_color_code([255, 128, 0]).to_string()));
        let loaded = read_dxf(&path).expect("read");
        assert_eq!(loaded.paths[0].layer, "Corte");
        assert_eq!(loaded.pens[loaded.paths[0].pen].color, [255, 128, 0]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn layer_names_drop_characters_dxf_rejects() {
        assert_eq!(sanitize_layer("a/b:c"), "a_b_c");
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
