//! Read an EzCad `.ezd` into pens and mark paths.

use std::fs;
use std::path::Path;

use super::{f64_field, huffman_decode, i32_field, point_field, utf16_lossy, Cursor};
use crate::geom::{Contour, Document, PathObj, Pen};
use crate::{Error, Result};

const CURVE: i32 = 1;
const RECT: i32 = 3;
const CIRCLE: i32 = 4;
const ELLIPSE: i32 = 5;
const POLYGON: i32 = 6;
const GROUP: i32 = 0x10;
const HATCH: i32 = 0x20;
const COMBINE: i32 = 0x30;
const IMAGE: i32 = 0x40;
const VECTOR_FILE: i32 = 0x50;
const SPIRAL: i32 = 0x60;
const TEXT: i32 = 0x800;

/// Read pens, text notes, and mark paths from an `.ezd` file.
///
/// # Errors
///
/// Returns an error when the file is not an EzCad Unicode drawing or its
/// object stream cannot be parsed.
pub fn read_ezd(path: &Path) -> Result<Document> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut cursor = Cursor::new(&bytes);
    let magic = utf16_lossy(cursor.take(16)?);
    if magic != "EZCADUNI" {
        return Err(Error::Format(
            "not an EzCad file (missing EZCADUNI)".to_owned(),
        ));
    }
    let _version_a = cursor.i32()?;
    let _version_b = cursor.i32()?;
    cursor.take(60)?;
    cursor.take(60)?;
    cursor.take(60)?;
    cursor.take(140)?;
    let preview_at = cursor.i32()?;
    let _v1 = cursor.i32()?;
    let pens_at = cursor.i32()?;
    let font_at = cursor.i32()?;
    let _v4 = cursor.i32()?;
    let vectors_at = cursor.i32()?;
    let _prevectors_at = cursor.i32()?;

    let mut doc = Document::new(file_stem(path));
    if pens_at > 0 {
        doc.pens = read_pens(&bytes, pens_at as usize)?;
    }
    if font_at > 0 {
        let _fonts = read_fonts(&bytes, font_at as usize)?;
    }
    if preview_at > 0 {
        let _preview = read_preview(&bytes, preview_at as usize)?;
    }
    if vectors_at > 0 {
        read_vectors(&bytes, vectors_at as usize, &mut doc)?;
    }
    Ok(doc)
}

fn file_stem(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("drawing")
        .to_owned()
}

fn read_fonts(bytes: &[u8], at: usize) -> Result<Vec<String>> {
    let mut cursor = Cursor {
        data: bytes,
        pos: at,
    };
    let count = cursor.i32()?;
    if !(0..=256).contains(&count) {
        return Err(Error::Format(format!("font count {count} is not usable")));
    }
    let mut fonts = Vec::with_capacity(count as usize);
    for _ in 0..count {
        fonts.push(utf16_lossy(cursor.take(100)?));
    }
    Ok(fonts)
}

fn read_preview(bytes: &[u8], at: usize) -> Result<()> {
    let mut cursor = Cursor {
        data: bytes,
        pos: at,
    };
    let _unknown = cursor.i32()?;
    let width = cursor.i32()?;
    let height = cursor.i32()?;
    if width <= 0 || height <= 0 || width > 4000 || height > 4000 {
        return Err(Error::Format("preview image size is not usable".to_owned()));
    }
    cursor.take(12)?;
    cursor.take((width as usize) * (height as usize) * 4)?;
    Ok(())
}

fn read_pens(bytes: &[u8], at: usize) -> Result<Vec<Pen>> {
    let mut cursor = Cursor {
        data: bytes,
        pos: at,
    };
    let count = cursor.i32()?;
    let array_at = cursor.i32()?;
    if !(1..=256).contains(&count) || array_at <= 0 {
        return Err(Error::Format("pen table is not usable".to_owned()));
    }
    cursor.pos = array_at as usize;
    let mut pens = Vec::with_capacity(count as usize);
    for index in 0..count {
        let fields = cursor.struct_fields(128)?;
        pens.push(pen_from_fields(index as usize, &fields));
    }
    while pens.len() < 256 {
        let index = pens.len();
        pens.push(Pen::new(index, [0, 0, 0]));
    }
    Ok(pens)
}

fn pen_from_fields(index: usize, fields: &[&[u8]]) -> Pen {
    let mut pen = Pen::new(index, [0, 0, 0]);
    if let Some(color) = fields.first().and_then(|bytes| i32_field(bytes)) {
        let color = color as u32;
        // EzCad stores COLORREF: 0x00BBGGRR.
        pen.color = [
            (color & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
            ((color >> 16) & 0xff) as u8,
        ];
    }
    if let Some(name) = fields.get(1) {
        let name = utf16_lossy(name);
        if !name.is_empty() {
            pen.name = name;
        }
    }
    if let Some(passes) = fields.get(4).and_then(|bytes| i32_field(bytes)) {
        pen.passes = passes.max(1);
    }
    if let Some(speed) = fields.get(5).and_then(|bytes| f64_field(bytes)) {
        pen.speed = speed;
    }
    if let Some(power) = fields.get(6).and_then(|bytes| f64_field(bytes)) {
        pen.power = power;
    }
    if let Some(freq) = fields.get(7).and_then(|bytes| i32_field(bytes)) {
        pen.frequency_khz = f64::from(freq) / 1000.0;
    }
    pen
}

fn read_vectors(bytes: &[u8], at: usize, doc: &mut Document) -> Result<()> {
    let mut cursor = Cursor {
        data: bytes,
        pos: at,
    };
    let uncompressed = cursor.u32()? as usize;
    let _unknown_2 = cursor.u32()?;
    let _compressed_len = cursor.u32()?;
    let _data_start = cursor.u32()?;
    let _unknown_5 = cursor.u32()?;
    if uncompressed > 64_000_000 {
        return Err(Error::Format(
            "vector section is larger than expected".to_owned(),
        ));
    }
    let decoded = huffman_decode(&mut cursor, uncompressed)?;
    let mut objects = Cursor::new(&decoded);
    let mut ordinal = 0_usize;
    while objects.remaining() >= 4 {
        let start = objects.pos;
        let kind = objects.i32()?;
        if kind == 0 {
            continue;
        }
        parse_typed(&mut objects, kind, doc, &mut ordinal).map_err(|err| {
            Error::Format(format!("{err} (object at decompressed byte {start})"))
        })?;
    }
    Ok(())
}

fn parse_typed(cursor: &mut Cursor<'_>, kind: i32, doc: &mut Document, ordinal: &mut usize) -> Result<()> {
    let header = cursor.struct_fields(64)?;
    let pen = header
        .first()
        .and_then(|bytes| i32_field(bytes))
        .unwrap_or(0)
        .clamp(0, 255) as usize;
    let label = header.get(3).map(|bytes| utf16_lossy(bytes)).unwrap_or_default();
    if matches!(kind, GROUP | HATCH | COMBINE | VECTOR_FILE | SPIRAL) {
        let children = cursor.i32()?;
        if !(0..=100_000).contains(&children) {
            return Err(Error::Format(format!("object has {children} children")));
        }
        for _ in 0..children {
            let child = cursor.i32()?;
            if child == 0 {
                break;
            }
            parse_typed(cursor, child, doc, ordinal)?;
        }
    }
    match kind {
        CURVE => parse_curve(cursor, doc, pen, &label, ordinal)?,
        RECT => parse_rect(cursor, doc, pen, &label, ordinal)?,
        CIRCLE => parse_circle(cursor, doc, pen, &label, ordinal)?,
        ELLIPSE => parse_ellipse(cursor, doc, pen, &label, ordinal)?,
        POLYGON => parse_polygon(cursor, doc, pen, &label, ordinal)?,
        TEXT => parse_text(cursor, doc)?,
        HATCH => parse_hatch_tail(cursor, doc, ordinal)?,
        IMAGE => skip_image(cursor)?,
        SPIRAL => {
            let _props = cursor.struct_fields(128)?;
            let child = cursor.i32()?;
            if child != 0 {
                parse_typed(cursor, child, doc, ordinal)?;
            }
        }
        VECTOR_FILE => {
            let _props = cursor.struct_fields(64)?;
        }
        GROUP | COMBINE => {}
        TIMER | INPUT | OUTPUT | AXIS | ENCODER => {
            let _props = cursor.struct_fields(128)?;
        }
        other => {
            return Err(Error::Format(format!("unknown object type {other:#x}")));
        }
    }
    Ok(())
}

const TIMER: i32 = 0x2000;
const INPUT: i32 = 0x3000;
const OUTPUT: i32 = 0x4000;
const AXIS: i32 = 0x5000;
const ENCODER: i32 = 0x6000;

fn parse_curve(
    cursor: &mut Cursor<'_>,
    doc: &mut Document,
    pen: usize,
    label: &str,
    ordinal: &mut usize,
) -> Result<()> {
    let count = cursor.u32()? as usize;
    let closed = cursor.u32()? != 0;
    if count > 200_000 {
        return Err(Error::Format(format!("curve has {count} contours")));
    }
    let mut contours = Vec::new();
    for _ in 0..count {
        let header = cursor.take(6)?;
        let curve_type = header[1];
        if curve_type == 0 {
            cursor.take(40)?;
            continue;
        }
        let point_count = cursor.i32()?;
        if !(0..=2_000_000).contains(&point_count) {
            return Err(Error::Format(format!(
                "curve contour has {point_count} points"
            )));
        }
        let mut raw = Vec::with_capacity(point_count as usize);
        for _ in 0..point_count {
            raw.push([cursor.f64()?, cursor.f64()?]);
        }
        let pts = sample_curve(curve_type, &raw);
        if pts.len() >= 2 {
            contours.push(Contour { closed, pts });
        }
    }
    push_path(doc, pen, label, "Curve", ordinal, contours);
    Ok(())
}

fn sample_curve(curve_type: u8, raw: &[[f64; 2]]) -> Vec<[f64; 2]> {
    match curve_type {
        2 => sample_quadratic(raw),
        3 => sample_cubic(raw),
        _ => raw.to_vec(),
    }
}

fn sample_quadratic(raw: &[[f64; 2]]) -> Vec<[f64; 2]> {
    if raw.len() < 3 {
        return raw.to_vec();
    }
    let mut pts = vec![raw[0]];
    let mut index = 1;
    let mut current = raw[0];
    while index + 1 < raw.len() {
        let control = raw[index];
        let end = raw[index + 1];
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let u = 1.0 - t;
            pts.push([
                u * u * current[0] + 2.0 * u * t * control[0] + t * t * end[0],
                u * u * current[1] + 2.0 * u * t * control[1] + t * t * end[1],
            ]);
        }
        current = end;
        index += 2;
    }
    pts
}

fn sample_cubic(raw: &[[f64; 2]]) -> Vec<[f64; 2]> {
    if raw.len() < 4 {
        return raw.to_vec();
    }
    let mut pts = vec![raw[0]];
    let mut index = 1;
    let mut current = raw[0];
    while index + 2 < raw.len() {
        let c1 = raw[index];
        let c2 = raw[index + 1];
        let end = raw[index + 2];
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let u = 1.0 - t;
            pts.push([
                u.powi(3) * current[0]
                    + 3.0 * u * u * t * c1[0]
                    + 3.0 * u * t * t * c2[0]
                    + t.powi(3) * end[0],
                u.powi(3) * current[1]
                    + 3.0 * u * u * t * c1[1]
                    + 3.0 * u * t * t * c2[1]
                    + t.powi(3) * end[1],
            ]);
        }
        current = end;
        index += 3;
    }
    pts
}

fn parse_rect(
    cursor: &mut Cursor<'_>,
    doc: &mut Document,
    pen: usize,
    label: &str,
    ordinal: &mut usize,
) -> Result<()> {
    let fields = cursor.struct_fields(32)?;
    if let (Some(min), Some(max)) = (
        fields.first().and_then(|bytes| point_field(bytes)),
        fields.get(1).and_then(|bytes| point_field(bytes)),
    ) {
        push_path(
            doc,
            pen,
            label,
            "Rectangle",
            ordinal,
            vec![Contour {
                closed: true,
                pts: vec![
                    min,
                    [max[0], min[1]],
                    max,
                    [min[0], max[1]],
                    min,
                ],
            }],
        );
    }
    Ok(())
}

fn parse_circle(
    cursor: &mut Cursor<'_>,
    doc: &mut Document,
    pen: usize,
    label: &str,
    ordinal: &mut usize,
) -> Result<()> {
    let fields = cursor.struct_fields(32)?;
    if let (Some(center), Some(radius)) = (
        fields.first().and_then(|bytes| point_field(bytes)),
        fields.get(1).and_then(|bytes| f64_field(bytes)),
    ) {
        push_path(
            doc,
            pen,
            label,
            "Circle",
            ordinal,
            vec![circle_contour(center, radius, radius)],
        );
    }
    Ok(())
}

fn parse_ellipse(
    cursor: &mut Cursor<'_>,
    doc: &mut Document,
    pen: usize,
    label: &str,
    ordinal: &mut usize,
) -> Result<()> {
    let fields = cursor.struct_fields(32)?;
    if let (Some(min), Some(max)) = (
        fields.get(1).and_then(|bytes| point_field(bytes)),
        fields.get(2).and_then(|bytes| point_field(bytes)),
    ) {
        let center = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
        let radius = [(max[0] - min[0]).abs() / 2.0, (max[1] - min[1]).abs() / 2.0];
        push_path(
            doc,
            pen,
            label,
            "Ellipse",
            ordinal,
            vec![circle_contour(center, radius[0], radius[1])],
        );
    }
    Ok(())
}

fn parse_polygon(
    cursor: &mut Cursor<'_>,
    doc: &mut Document,
    pen: usize,
    label: &str,
    ordinal: &mut usize,
) -> Result<()> {
    let fields = cursor.struct_fields(32)?;
    let sides = fields.get(7).and_then(|bytes| i32_field(bytes)).unwrap_or(0);
    if let (Some(min), Some(max)) = (
        fields.get(1).and_then(|bytes| point_field(bytes)),
        fields.get(2).and_then(|bytes| point_field(bytes)),
    ) {
        if (3..=64).contains(&sides) {
            let center = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
            let radius = [(max[0] - min[0]).abs() / 2.0, (max[1] - min[1]).abs() / 2.0];
            let mut pts = Vec::with_capacity(sides as usize + 1);
            for index in 0..=sides {
                let theta = std::f64::consts::TAU * f64::from(index) / f64::from(sides)
                    + std::f64::consts::FRAC_PI_2;
                pts.push([
                    center[0] + radius[0] * theta.cos(),
                    center[1] + radius[1] * theta.sin(),
                ]);
            }
            push_path(
                doc,
                pen,
                label,
                "Polygon",
                ordinal,
                vec![Contour { closed: true, pts }],
            );
        }
    }
    Ok(())
}

fn circle_contour(center: [f64; 2], rx: f64, ry: f64) -> Contour {
    let steps = 48;
    let mut pts = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let theta = std::f64::consts::TAU * (step as f64) / (steps as f64);
        pts.push([
            center[0] + rx * theta.cos(),
            center[1] + ry * theta.sin(),
        ]);
    }
    Contour { closed: true, pts }
}

fn parse_text(cursor: &mut Cursor<'_>, doc: &mut Document) -> Result<()> {
    let fields = cursor.struct_fields(400)?;
    if let Some(text) = fields.get(10) {
        let text = utf16_lossy(text);
        if !text.is_empty() {
            doc.notes.push(text);
        }
    }
    let count = cursor.i32()?;
    if !(0..=10_000).contains(&count) {
        return Err(Error::Format(format!("text object has {count} parts")));
    }
    for _ in 0..count {
        cursor.take(2)?;
        let _part = cursor.struct_fields(200)?;
        let _part = cursor.struct_fields(200)?;
    }
    cursor.i32()?;
    Ok(())
}

fn parse_hatch_tail(cursor: &mut Cursor<'_>, doc: &mut Document, ordinal: &mut usize) -> Result<()> {
    let _props = cursor.struct_fields(200)?;
    if cursor.remaining() < 4 {
        return Ok(());
    }
    let next = i32::from_le_bytes(cursor.data[cursor.pos..cursor.pos + 4].try_into().expect("4"));
    // The cached hatch group has no type code. Its header list length is 15.
    if next == 15 {
        let _header = cursor.struct_fields(64)?;
        let children = cursor.i32()?;
        if !(0..=100_000).contains(&children) {
            return Err(Error::Format(format!("hatch cache has {children} children")));
        }
        for _ in 0..children {
            let child = cursor.i32()?;
            if child == 0 {
                break;
            }
            parse_typed(cursor, child, doc, ordinal)?;
        }
    }
    Ok(())
}

fn skip_image(cursor: &mut Cursor<'_>) -> Result<()> {
    let _props = cursor.struct_fields(400)?;
    let size = {
        let marker = cursor.take(6)?;
        i32::from_le_bytes(marker[2..6].try_into().expect("4 bytes"))
    };
    if size < 6 || size > 64_000_000 {
        return Err(Error::Format("image object is not usable".to_owned()));
    }
    cursor.take((size - 6) as usize)?;
    Ok(())
}

fn push_path(
    doc: &mut Document,
    pen: usize,
    label: &str,
    kind: &str,
    ordinal: &mut usize,
    contours: Vec<Contour>,
) {
    if contours.is_empty() {
        return;
    }
    *ordinal += 1;
    let name = if label.is_empty() {
        format!("{kind} {ordinal}")
    } else {
        label.to_owned()
    };
    doc.paths.push(PathObj {
        name,
        layer: String::new(),
        pen,
        contours,
    });
}
