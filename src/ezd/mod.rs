//! EzCad 2 `.ezd` reader and writer.
//!
//! The vector section is a Huffman-compressed stream of objects. Curves are the
//! mark paths. This module reads those paths and writes the EzCad 2.14 Unicode
//! layout: header, pens, preview, and compressed curves.

mod read;
mod write;

pub use read::read_ezd;
pub use write::write_ezd;

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> crate::Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| {
                crate::Error::Format(format!(
                    "ezd ended early at byte {} while reading {len} bytes",
                    self.pos
                ))
            })?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn i32(&mut self) -> crate::Result<i32> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn u32(&mut self) -> crate::Result<u32> {
        Ok(self.i32()? as u32)
    }

    fn f64(&mut self) -> crate::Result<f64> {
        let bytes = self.take(8)?;
        Ok(f64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }

    fn struct_fields(&mut self, max_count: i32) -> crate::Result<Vec<&'a [u8]>> {
        let count = self.i32()?;
        if !(0..=max_count).contains(&count) {
            return Err(crate::Error::Format(format!(
                "unexpected list length {count} at byte {}",
                self.pos
            )));
        }
        let mut fields = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let len = self.i32()?;
            if !(0..=8_000_000).contains(&len) {
                return Err(crate::Error::Format(format!(
                    "unexpected field length {len} at byte {}",
                    self.pos
                )));
            }
            fields.push(self.take(len as usize)?);
        }
        Ok(fields)
    }
}

fn utf16_lossy(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

fn i32_field(bytes: &[u8]) -> Option<i32> {
    bytes
        .try_into()
        .ok()
        .map(i32::from_le_bytes)
}

fn f64_field(bytes: &[u8]) -> Option<f64> {
    bytes
        .try_into()
        .ok()
        .map(f64::from_le_bytes)
}

fn point_field(bytes: &[u8]) -> Option<[f64; 2]> {
    if bytes.len() != 16 {
        return None;
    }
    let x = f64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes"));
    let y = f64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
    Some([x, y])
}

fn huffman_decode(cursor: &mut Cursor<'_>, uncompressed: usize) -> crate::Result<Vec<u8>> {
    use std::collections::HashMap;

    let table_len = u16::from_le_bytes(le4_u16(cursor.take(2)?)) as usize;
    let mut codes: HashMap<(u8, u32), u8> = HashMap::with_capacity(table_len);
    let mut max_len = 0_u8;
    for _ in 0..table_len {
        let symbol = cursor.take(1)?[0];
        let bits = u32::from_le_bytes(le4(cursor.take(4)?));
        let len = u16::from_le_bytes(le4_u16(cursor.take(2)?));
        if len == 0 || len > 32 {
            return Err(crate::Error::Format(
                "huffman code length is out of range".to_owned(),
            ));
        }
        let len = len as u8;
        max_len = max_len.max(len);
        let mask = if len == 32 { u32::MAX } else { (1_u32 << len) - 1 };
        codes.insert((len, bits & mask), symbol);
    }
    let packed = cursor.take(cursor.remaining())?;
    let mut out = Vec::with_capacity(uncompressed);
    let mut acc = 0_u32;
    let mut acc_len = 0_u8;
    let mut byte_index = 0_usize;
    let mut bit_index = 0_u8;
    while out.len() < uncompressed {
        if byte_index >= packed.len() {
            return Err(crate::Error::Format(
                "huffman stream ended before the vector data".to_owned(),
            ));
        }
        let bit = (packed[byte_index] >> (7 - bit_index)) & 1;
        bit_index += 1;
        if bit_index == 8 {
            bit_index = 0;
            byte_index += 1;
        }
        acc = (acc << 1) | u32::from(bit);
        acc_len += 1;
        if let Some(symbol) = codes.get(&(acc_len, acc)) {
            out.push(*symbol);
            acc = 0;
            acc_len = 0;
        } else if acc_len > max_len {
            return Err(crate::Error::Format(
                "huffman stream does not match its table".to_owned(),
            ));
        }
    }
    Ok(out)
}

fn le4(bytes: &[u8]) -> [u8; 4] {
    bytes.try_into().unwrap_or([0; 4])
}

fn le4_u16(bytes: &[u8]) -> [u8; 2] {
    bytes.try_into().unwrap_or([0; 2])
}

fn huffman_encode(data: &[u8]) -> Vec<u8> {
    // Identity codes: every byte is stored as itself, MSB first. EzCad reads
    // the table, so the stream does not have to be smaller than the input.
    let mut out = Vec::with_capacity(2 + 256 * 7 + data.len() + 1);
    out.extend_from_slice(&256_u16.to_le_bytes());
    for symbol in 0..256_u16 {
        out.push(symbol as u8);
        out.extend_from_slice(&u32::from(symbol).to_le_bytes());
        out.extend_from_slice(&8_u16.to_le_bytes());
    }
    out.extend_from_slice(data);
    out
}
