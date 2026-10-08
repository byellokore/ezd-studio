//! Write a drawing as an EzCad 2 `.ezd` file.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::huffman_encode;
use crate::geom::{Contour, Document, Pen};
use crate::{Error, Result};

const PEN_TEMPLATE: &[u8] = include_bytes!("pen_template.bin");
const PREVIEW: i32 = 200;

/// Write `doc` as an EzCad Unicode `.ezd`.
///
/// Curves are stored as polylines. Pen speed, power, frequency, passes, color,
/// and name are written into the 256-pen table.
///
/// # Errors
///
/// Returns an error when the destination cannot be created.
pub fn write_ezd(path: &Path, doc: &Document) -> Result<()> {
    let bytes = encode(doc);
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

fn encode(doc: &Document) -> Vec<u8> {
    let vectors = encode_vectors(doc);
    let compressed = huffman_encode(&vectors);
    let preview = encode_preview(doc);
    let fonts = 0_i32.to_le_bytes().to_vec();
    let pens = encode_pens(doc);

    let mut header = Vec::with_capacity(344);
    header.extend(utf16_fixed("EZCADUNI", 8));
    header.extend_from_slice(&0_i32.to_le_bytes());
    header.extend_from_slice(&2001_i32.to_le_bytes());
    header.resize(344, 0);

    let seek_at = header.len();
    let mut out = header;
    out.resize(seek_at + 28 + 96, 0);

    let preview_at = i32::try_from(out.len()).unwrap_or(0);
    out.extend_from_slice(&preview);
    let font_at = i32::try_from(out.len()).unwrap_or(0);
    out.extend_from_slice(&fonts);
    let pens_at = i32::try_from(out.len()).unwrap_or(0);
    let pen_array_at = pens_at + 8;
    out.extend_from_slice(&256_i32.to_le_bytes());
    out.extend_from_slice(&pen_array_at.to_le_bytes());
    out.extend_from_slice(&pens);
    let prevectors_at = i32::try_from(out.len()).unwrap_or(0);
    out.resize(out.len() + 400, 0);
    let vectors_at = i32::try_from(out.len()).unwrap_or(0);
    let data_start = vectors_at + 20;
    let payload_len = i32::try_from(compressed.len().saturating_sub(2 + 256 * 7)).unwrap_or(0);
    // Word 2 sits inside the 16 bytes that word 5 covers, so the content
    // checksum has to be finished before the header checksum.
    let content_crc = u32::from(crc16_x25(&vectors));
    let mut vector_header = [0_u8; 20];
    vector_header[0..4].copy_from_slice(&(vectors.len() as u32).to_le_bytes());
    vector_header[4..8].copy_from_slice(&content_crc.to_le_bytes());
    vector_header[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    vector_header[12..16].copy_from_slice(&(data_start as u32).to_le_bytes());
    let header_crc = u32::from(crc16_x25(&vector_header[..16]));
    vector_header[16..20].copy_from_slice(&header_crc.to_le_bytes());
    out.extend_from_slice(&vector_header);
    out.extend_from_slice(&compressed);

    put_i32(&mut out, seek_at, preview_at);
    put_i32(&mut out, seek_at + 4, 0);
    put_i32(&mut out, seek_at + 8, pens_at);
    put_i32(&mut out, seek_at + 12, font_at);
    put_i32(&mut out, seek_at + 16, 0);
    put_i32(&mut out, seek_at + 20, vectors_at);
    put_i32(&mut out, seek_at + 24, prevectors_at);
    out
}

fn put_i32(buf: &mut [u8], at: usize, value: i32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// CRC-16/X-25: reflected polynomial 0x8408, init 0xFFFF, xorout 0xFFFF.
///
/// EzCad stores this in the low 16 bits of vector-header words 2 and 5.
fn crc16_x25(data: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for &byte in data {
        let index = usize::from((crc ^ u16::from(byte)) & 0xFF);
        crc = CRC16_X25[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFF
}

const CRC16_X25: [u16; 256] = {
    let mut table = [0_u16; 256];
    let mut index = 0_u16;
    while index < 256 {
        let mut value = index;
        let mut bit = 0;
        while bit < 8 {
            if value & 1 == 1 {
                value = (value >> 1) ^ 0x8408;
            } else {
                value >>= 1;
            }
            bit += 1;
        }
        table[index as usize] = value;
        index += 1;
    }
    table
};

fn encode_vectors(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    for path in &doc.paths {
        for contour in &path.contours {
            if contour.pts.len() < 2 {
                continue;
            }
            write_curve(&mut out, path.pen, &path.name, contour);
        }
    }
    out.extend_from_slice(&0_i32.to_le_bytes());
    out
}

fn write_curve(out: &mut Vec<u8>, pen: usize, name: &str, contour: &Contour) {
    out.extend_from_slice(&1_i32.to_le_bytes());
    let mut fields: Vec<Vec<u8>> = Vec::with_capacity(15);
    fields.push((pen as i32).to_le_bytes().to_vec());
    fields.push(1_u16.to_le_bytes().to_vec());
    fields.push(0_u16.to_le_bytes().to_vec());
    fields.push(utf16_z(trim_name(name)));
    fields.push(1_i32.to_le_bytes().to_vec());
    fields.push(0_u16.to_le_bytes().to_vec());
    fields.push(0_u16.to_le_bytes().to_vec());
    fields.push(0_u16.to_le_bytes().to_vec());
    fields.push(0_u16.to_le_bytes().to_vec());
    fields.push(1_i32.to_le_bytes().to_vec());
    fields.push(1_i32.to_le_bytes().to_vec());
    fields.push(10_f64.to_le_bytes().to_vec());
    fields.push(10_f64.to_le_bytes().to_vec());
    let anchor = contour.pts[0];
    let mut point = Vec::with_capacity(16);
    point.extend_from_slice(&anchor[0].to_le_bytes());
    point.extend_from_slice(&anchor[1].to_le_bytes());
    fields.push(point);
    fields.push(0_f64.to_le_bytes().to_vec());
    write_struct(out, &fields);

    out.extend_from_slice(&1_u32.to_le_bytes());
    out.extend_from_slice(&u32::from(contour.closed).to_le_bytes());
    out.push(0);
    out.push(1);
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    let mut pts = contour.pts.clone();
    if contour.closed {
        let first = pts[0];
        let last = pts[pts.len() - 1];
        let dx = first[0] - last[0];
        let dy = first[1] - last[1];
        if dx * dx + dy * dy > 1e-8 {
            pts.push(first);
        }
    }
    out.extend_from_slice(&(pts.len() as i32).to_le_bytes());
    for pt in pts {
        out.extend_from_slice(&pt[0].to_le_bytes());
        out.extend_from_slice(&pt[1].to_le_bytes());
    }
}

fn write_struct(out: &mut Vec<u8>, fields: &[Vec<u8>]) {
    out.extend_from_slice(&(fields.len() as i32).to_le_bytes());
    for field in fields {
        out.extend_from_slice(&(field.len() as i32).to_le_bytes());
        out.extend_from_slice(field);
    }
}

fn trim_name(name: &str) -> &str {
    match name.char_indices().nth(40) {
        Some((index, _)) => &name[..index],
        None => name,
    }
}

fn encode_pens(doc: &Document) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 * PEN_TEMPLATE.len());
    for index in 0..256 {
        let pen = doc.pens.get(index).cloned().unwrap_or_else(|| Pen::new(index, [0, 0, 0]));
        out.extend_from_slice(&pen_record(&pen));
    }
    out
}

fn pen_record(pen: &Pen) -> Vec<u8> {
    let mut fields = read_template_fields(PEN_TEMPLATE);
    if let Some(color) = fields.first_mut() {
        let bgr = i32::from(pen.color[0])
            | (i32::from(pen.color[1]) << 8)
            | (i32::from(pen.color[2]) << 16);
        color.clone_from(&bgr.to_le_bytes().to_vec());
    }
    if let Some(name) = fields.get_mut(1) {
        *name = utf16_z(&pen.name);
    }
    if let Some(passes) = fields.get_mut(4) {
        *passes = pen.passes.max(1).to_le_bytes().to_vec();
    }
    if let Some(speed) = fields.get_mut(5) {
        *speed = pen.speed.to_le_bytes().to_vec();
    }
    if let Some(power) = fields.get_mut(6) {
        *power = pen.power.to_le_bytes().to_vec();
    }
    if let Some(freq) = fields.get_mut(7) {
        let hz = (pen.frequency_khz * 1000.0).round() as i32;
        *freq = hz.to_le_bytes().to_vec();
    }
    let mut out = Vec::new();
    write_struct(&mut out, &fields);
    out
}

fn read_template_fields(blob: &[u8]) -> Vec<Vec<u8>> {
    let count = i32::from_le_bytes(blob[0..4].try_into().unwrap_or([0; 4])) as usize;
    let mut pos = 4;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 4 > blob.len() {
            break;
        }
        let len = i32::from_le_bytes(blob[pos..pos + 4].try_into().unwrap_or([0; 4])) as usize;
        pos += 4;
        let end = (pos + len).min(blob.len());
        fields.push(blob[pos..end].to_vec());
        pos = end;
    }
    fields
}

fn encode_preview(doc: &Document) -> Vec<u8> {
    let mut pixels = vec![0xff_u8; (PREVIEW * PREVIEW * 4) as usize];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 0;
    }
    let bounds = doc.bounds();
    if let Some(bounds) = bounds {
        let width = bounds.width().max(0.001);
        let height = bounds.height().max(0.001);
        let scale = (f64::from(PREVIEW - 16) / width).min(f64::from(PREVIEW - 16) / height);
        for path in &doc.paths {
            for contour in &path.contours {
                let mut prev: Option<(i32, i32)> = None;
                for pt in &contour.pts {
                    let x = ((pt[0] - bounds.min_x) * scale + 8.0).round() as i32;
                    let y = (f64::from(PREVIEW) - 8.0 - (pt[1] - bounds.min_y) * scale).round() as i32;
                    if let Some(last) = prev {
                        draw_line(&mut pixels, last, (x, y));
                    }
                    prev = Some((x, y));
                }
            }
        }
    }
    let mut out = Vec::with_capacity(24 + pixels.len());
    out.extend_from_slice(&0_i32.to_le_bytes());
    out.extend_from_slice(&PREVIEW.to_le_bytes());
    out.extend_from_slice(&PREVIEW.to_le_bytes());
    out.extend_from_slice(&800_i32.to_le_bytes());
    out.extend_from_slice(&0x0020_0001_i32.to_le_bytes());
    out.extend_from_slice(&0_i32.to_le_bytes());
    out.extend_from_slice(&pixels);
    out
}

fn draw_line(pixels: &mut [u8], start: (i32, i32), end: (i32, i32)) {
    let (mut x0, mut y0) = start;
    let (x1, y1) = end;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        plot(pixels, x0, y0);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn plot(pixels: &mut [u8], x: i32, y: i32) {
    if x < 0 || y < 0 || x >= PREVIEW || y >= PREVIEW {
        return;
    }
    let index = ((y * PREVIEW + x) * 4) as usize;
    pixels[index] = 0x22;
    pixels[index + 1] = 0x22;
    pixels[index + 2] = 0x22;
}

fn utf16_fixed(text: &str, units: usize) -> Vec<u8> {
    let mut out = vec![0_u8; units * 2];
    for (index, unit) in text.encode_utf16().take(units).enumerate() {
        let bytes = unit.to_le_bytes();
        out[index * 2] = bytes[0];
        out[index * 2 + 1] = bytes[1];
    }
    out
}

fn utf16_z(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PathObj;
    use crate::read_ezd;

    #[test]
    fn round_trip_square_keeps_the_corners() {
        let doc = square_doc();
        let path = std::env::temp_dir().join("ezd-studio-square.ezd");
        write_ezd(&path, &doc).expect("write");
        let loaded = read_ezd(&path).expect("read");
        let bounds = loaded.bounds().expect("bounds");
        assert!((bounds.min_x - -10.0).abs() < 1e-6);
        assert!((bounds.max_x - 10.0).abs() < 1e-6);
        assert!((bounds.min_y - -5.0).abs() < 1e-6);
        assert!((bounds.max_y - 5.0).abs() < 1e-6);
        assert!((loaded.pens[2].power - 33.0).abs() < 1e-6);
        assert!((loaded.pens[2].speed - 800.0).abs() < 1e-6);
        assert!((loaded.pens[2].frequency_khz - 40.0).abs() < 1e-6);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn crc16_x25_matches_the_catalog_check_value() {
        // CRC-16/X-25 check value and residue from the CRC catalogue.
        assert_eq!(crc16_x25(b"123456789"), 0x906E);
        assert!(x25_residue_matches(b"123456789", 0x906E));
    }

    #[test]
    fn written_vector_header_carries_both_checksums() {
        let doc = square_doc();
        let bytes = encode(&doc);
        let vectors_at = le_u32(&bytes, 364) as usize;
        let header = &bytes[vectors_at..vectors_at + 20];
        let vectors = encode_vectors(&doc);
        let content_crc = le_u32(header, 4);
        let header_crc = le_u32(header, 16);
        assert_eq!(le_u32(header, 0), vectors.len() as u32);
        assert_eq!(content_crc, u32::from(crc16_x25(&vectors)));
        assert_eq!(header_crc, u32::from(crc16_x25(&header[..16])));
        assert_ne!(content_crc, 0);
        assert_ne!(header_crc, 0);
        assert!(x25_residue_matches(&vectors, content_crc as u16));
        assert!(x25_residue_matches(&header[..16], header_crc as u16));
    }

    #[test]
    fn sample_autosave_checksums_match_ezcad() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../AUTOSAVE.EZD");
        let bytes = fs::read(&path).expect("sample autosave");
        let vectors_at = le_u32(&bytes, 364) as usize;
        let header = &bytes[vectors_at..vectors_at + 20];
        let uncompressed = le_u32(header, 0) as usize;
        let content_crc = le_u32(header, 4);
        let header_crc = le_u32(header, 16);
        assert_eq!(header_crc, u32::from(crc16_x25(&header[..16])));
        assert!(x25_residue_matches(&header[..16], header_crc as u16));
        let mut cursor = super::super::Cursor {
            data: &bytes,
            pos: vectors_at + 20,
        };
        let decoded = super::super::huffman_decode(&mut cursor, uncompressed).expect("decode");
        assert_eq!(decoded.len(), uncompressed);
        assert_eq!(content_crc, u32::from(crc16_x25(&decoded)));
        assert!(x25_residue_matches(&decoded, content_crc as u16));
    }

    fn square_doc() -> Document {
        let mut doc = Document::new("square");
        doc.paths.push(PathObj {
            name: "Box".into(),
            layer: String::new(),
            pen: 2,
            contours: vec![Contour {
                closed: true,
                pts: vec![[-10.0, -5.0], [10.0, -5.0], [10.0, 5.0], [-10.0, 5.0]],
            }],
        });
        doc.pens[2].power = 33.0;
        doc.pens[2].speed = 800.0;
        doc.pens[2].frequency_khz = 40.0;
        doc
    }

    fn le_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    /// EzCad's check: fold the stored CRC in after the data, with no final XOR,
    /// and require the residue 0xF0B8.
    fn x25_residue_matches(data: &[u8], stored: u16) -> bool {
        let mut crc = 0xFFFF_u16;
        for byte in data.iter().copied().chain(stored.to_le_bytes()) {
            let index = usize::from((crc ^ u16::from(byte)) & 0xFF);
            crc = CRC16_X25[index] ^ (crc >> 8);
        }
        crc == 0xF0B8
    }
}
