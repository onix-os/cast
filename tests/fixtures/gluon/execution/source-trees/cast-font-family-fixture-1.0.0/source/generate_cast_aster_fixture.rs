use std::{env, fs, path::Path};

const FAMILY: &str = "Cast Aster Fixture";
const COPYRIGHT: &str = "Copyright 2026 Cast Fixture Authors";
const LICENSE: &str = "This Font Software is licensed under the SIL Open Font License, Version 1.1.";
const LICENSE_URL: &str = "https://openfontlicense.org";
const CREATED_1904_SECONDS: u64 = 3_782_844_800;

#[derive(Clone, Copy)]
enum Style {
    Regular,
    Bold,
}

impl Style {
    const fn name(self) -> &'static str {
        match self {
            Self::Regular => "Regular",
            Self::Bold => "Bold",
        }
    }

    const fn weight(self) -> u16 {
        match self {
            Self::Regular => 400,
            Self::Bold => 700,
        }
    }

    const fn mac_style(self) -> u16 {
        match self {
            Self::Regular => 0,
            Self::Bold => 1,
        }
    }

    const fn selection(self) -> u16 {
        let typo_metrics_and_wws = 0x0080 | 0x0100;
        match self {
            Self::Regular => typo_metrics_and_wws | 0x0040,
            Self::Bold => typo_metrics_and_wws | 0x0020,
        }
    }

    fn postscript_name(self) -> String {
        format!("CastAsterFixture-{}", self.name())
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn align_four(bytes: &mut Vec<u8>) {
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
}

fn table_checksum(bytes: &[u8]) -> u32 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks(4) {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

fn head(style: Style) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(54);
    push_u32(&mut bytes, 0x0001_0000);
    push_u32(&mut bytes, 0x0001_0000);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0x5f0f_3cf5);
    push_u16(&mut bytes, 0x000b);
    push_u16(&mut bytes, 1000);
    push_u64(&mut bytes, CREATED_1904_SECONDS);
    push_u64(&mut bytes, CREATED_1904_SECONDS);
    push_i16(&mut bytes, 0);
    push_i16(&mut bytes, 0);
    push_i16(&mut bytes, 700);
    push_i16(&mut bytes, 700);
    push_u16(&mut bytes, style.mac_style());
    push_u16(&mut bytes, 8);
    push_i16(&mut bytes, 2);
    push_i16(&mut bytes, 1);
    push_i16(&mut bytes, 0);
    assert_eq!(bytes.len(), 54);
    bytes
}

fn hhea() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(36);
    push_u32(&mut bytes, 0x0001_0000);
    push_i16(&mut bytes, 800);
    push_i16(&mut bytes, -200);
    push_i16(&mut bytes, 0);
    push_u16(&mut bytes, 700);
    push_i16(&mut bytes, 0);
    push_i16(&mut bytes, 0);
    push_i16(&mut bytes, 700);
    push_i16(&mut bytes, 1);
    push_i16(&mut bytes, 0);
    push_i16(&mut bytes, 0);
    for _ in 0..4 {
        push_i16(&mut bytes, 0);
    }
    push_i16(&mut bytes, 0);
    push_u16(&mut bytes, 4);
    assert_eq!(bytes.len(), 36);
    bytes
}

fn maxp() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    push_u32(&mut bytes, 0x0001_0000);
    push_u16(&mut bytes, 4);
    for value in [4, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        push_u16(&mut bytes, value);
    }
    assert_eq!(bytes.len(), 32);
    bytes
}

fn os2(style: Style) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(96);
    push_u16(&mut bytes, 4);
    push_i16(&mut bytes, 588);
    push_u16(&mut bytes, style.weight());
    push_u16(&mut bytes, 5);
    push_u16(&mut bytes, 0);
    for value in [650, 600, 0, 75, 650, 600, 0, 350, 50, 250] {
        push_i16(&mut bytes, value);
    }
    push_i16(&mut bytes, 0);
    bytes.extend_from_slice(&[
        2,
        11,
        if matches!(style, Style::Bold) { 8 } else { 5 },
        3,
        2,
        2,
        3,
        2,
        2,
        4,
    ]);
    push_u32(&mut bytes, 1);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    bytes.extend_from_slice(b"CAST");
    push_u16(&mut bytes, style.selection());
    push_u16(&mut bytes, 0x0020);
    push_u16(&mut bytes, 0x0042);
    push_i16(&mut bytes, 800);
    push_i16(&mut bytes, -200);
    push_i16(&mut bytes, 0);
    push_u16(&mut bytes, 800);
    push_u16(&mut bytes, 200);
    push_u32(&mut bytes, 1);
    push_u32(&mut bytes, 0);
    push_i16(&mut bytes, 500);
    push_i16(&mut bytes, 700);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0x0020);
    push_u16(&mut bytes, 1);
    assert_eq!(bytes.len(), 96);
    bytes
}

fn hmtx() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    for (advance, side_bearing) in [(600, 50), (350, 0), (700, 0), (700, 0)] {
        push_u16(&mut bytes, advance);
        push_i16(&mut bytes, side_bearing);
    }
    bytes
}

fn simple_glyph(points: &[(i16, i16)]) -> Vec<u8> {
    assert!(!points.is_empty());
    let mut bytes = Vec::new();
    let x_min = points.iter().map(|point| point.0).min().unwrap();
    let y_min = points.iter().map(|point| point.1).min().unwrap();
    let x_max = points.iter().map(|point| point.0).max().unwrap();
    let y_max = points.iter().map(|point| point.1).max().unwrap();
    push_i16(&mut bytes, 1);
    push_i16(&mut bytes, x_min);
    push_i16(&mut bytes, y_min);
    push_i16(&mut bytes, x_max);
    push_i16(&mut bytes, y_max);
    push_u16(&mut bytes, u16::try_from(points.len() - 1).unwrap());
    push_u16(&mut bytes, 0);
    bytes.extend(std::iter::repeat_n(0x01, points.len()));
    let mut previous = 0_i16;
    for point in points {
        push_i16(&mut bytes, point.0 - previous);
        previous = point.0;
    }
    previous = 0;
    for point in points {
        push_i16(&mut bytes, point.1 - previous);
        previous = point.1;
    }
    bytes
}

fn empty_glyph() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(10);
    for _ in 0..5 {
        push_i16(&mut bytes, 0);
    }
    bytes
}

fn glyf_and_loca(style: Style) -> (Vec<u8>, Vec<u8>) {
    let a_left = if matches!(style, Style::Bold) { 0 } else { 50 };
    let a_right = if matches!(style, Style::Bold) { 700 } else { 650 };
    let b_inset = if matches!(style, Style::Bold) { 25 } else { 75 };
    let glyphs = [
        simple_glyph(&[(50, 0), (50, 700), (550, 700), (550, 0)]),
        empty_glyph(),
        simple_glyph(&[(a_left, 0), (350, 700), (a_right, 0)]),
        simple_glyph(&[(b_inset, 0), (b_inset, 700), (650, 700), (650, 0)]),
    ];
    let mut glyf = Vec::new();
    let mut offsets = Vec::with_capacity(glyphs.len() + 1);
    for glyph in glyphs {
        offsets.push(u32::try_from(glyf.len()).unwrap());
        glyf.extend_from_slice(&glyph);
        align_four(&mut glyf);
    }
    offsets.push(u32::try_from(glyf.len()).unwrap());
    let mut loca = Vec::with_capacity(offsets.len() * 4);
    for offset in offsets {
        push_u32(&mut loca, offset);
    }
    (glyf, loca)
}

fn cmap() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(52);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 1);
    push_u16(&mut bytes, 3);
    push_u16(&mut bytes, 1);
    push_u32(&mut bytes, 12);
    push_u16(&mut bytes, 4);
    push_u16(&mut bytes, 40);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 6);
    push_u16(&mut bytes, 4);
    push_u16(&mut bytes, 1);
    push_u16(&mut bytes, 2);
    for value in [0x0020, 0x0042, 0xffff] {
        push_u16(&mut bytes, value);
    }
    push_u16(&mut bytes, 0);
    for value in [0x0020, 0x0041, 0xffff] {
        push_u16(&mut bytes, value);
    }
    for value in [-31_i16, -63_i16, 1_i16] {
        push_i16(&mut bytes, value);
    }
    for _ in 0..3 {
        push_u16(&mut bytes, 0);
    }
    assert_eq!(bytes.len(), 52);
    bytes
}

fn utf16be(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

fn name(style: Style) -> Vec<u8> {
    let full_name = format!("{FAMILY} {}", style.name());
    let unique_id = format!("Cast Fixture Authors:{full_name}:1.000");
    let values = [
        (0_u16, COPYRIGHT.to_owned()),
        (1, FAMILY.to_owned()),
        (2, style.name().to_owned()),
        (3, unique_id),
        (4, full_name),
        (5, "Version 1.000".to_owned()),
        (6, style.postscript_name()),
        (13, LICENSE.to_owned()),
        (14, LICENSE_URL.to_owned()),
    ];
    let mut storage = Vec::new();
    let mut records = Vec::with_capacity(values.len());
    for (name_id, value) in values {
        let encoded = utf16be(&value);
        records.push((name_id, encoded.len(), storage.len()));
        storage.extend_from_slice(&encoded);
    }
    let mut bytes = Vec::new();
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, u16::try_from(records.len()).unwrap());
    push_u16(&mut bytes, u16::try_from(6 + records.len() * 12).unwrap());
    for (name_id, length, offset) in records {
        push_u16(&mut bytes, 3);
        push_u16(&mut bytes, 1);
        push_u16(&mut bytes, 0x0409);
        push_u16(&mut bytes, name_id);
        push_u16(&mut bytes, u16::try_from(length).unwrap());
        push_u16(&mut bytes, u16::try_from(offset).unwrap());
    }
    bytes.extend_from_slice(&storage);
    bytes
}

fn post() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    push_u32(&mut bytes, 0x0003_0000);
    push_u32(&mut bytes, 0);
    push_i16(&mut bytes, -100);
    push_i16(&mut bytes, 50);
    for _ in 0..5 {
        push_u32(&mut bytes, 0);
    }
    assert_eq!(bytes.len(), 32);
    bytes
}

fn build_font(style: Style) -> Vec<u8> {
    let (glyf, loca) = glyf_and_loca(style);
    let mut tables = vec![
        (*b"OS/2", os2(style)),
        (*b"cmap", cmap()),
        (*b"glyf", glyf),
        (*b"head", head(style)),
        (*b"hhea", hhea()),
        (*b"hmtx", hmtx()),
        (*b"loca", loca),
        (*b"maxp", maxp()),
        (*b"name", name(style)),
        (*b"post", post()),
    ];
    tables.sort_by_key(|table| table.0);
    let table_count = u16::try_from(tables.len()).unwrap();
    let directory_bytes = 12 + tables.len() * 16;
    let mut offset = directory_bytes;
    let mut records = Vec::with_capacity(tables.len());
    for (tag, data) in &tables {
        records.push((*tag, table_checksum(data), offset, data.len()));
        offset += (data.len() + 3) & !3;
    }

    let mut font = Vec::with_capacity(offset);
    push_u32(&mut font, 0x0001_0000);
    push_u16(&mut font, table_count);
    push_u16(&mut font, 128);
    push_u16(&mut font, 3);
    push_u16(&mut font, table_count * 16 - 128);
    for (tag, checksum, table_offset, length) in &records {
        font.extend_from_slice(tag);
        push_u32(&mut font, *checksum);
        push_u32(&mut font, u32::try_from(*table_offset).unwrap());
        push_u32(&mut font, u32::try_from(*length).unwrap());
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        align_four(&mut font);
    }
    assert_eq!(font.len(), offset);
    let head_offset = records
        .iter()
        .find(|record| record.0 == *b"head")
        .map(|record| record.2)
        .unwrap();
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(table_checksum(&font));
    font[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    assert_eq!(table_checksum(&font), 0xb1b0_afba);
    font
}

fn write_font(output: &Path, style: Style) -> std::io::Result<()> {
    fs::write(
        output.join(format!("CastAsterFixture-{}.ttf", style.name())),
        build_font(style),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os();
    let program = arguments.next().unwrap_or_default();
    let Some(output) = arguments.next() else {
        return Err(format!("usage: {} <output-directory>", Path::new(&program).display()).into());
    };
    if arguments.next().is_some() {
        return Err(format!("usage: {} <output-directory>", Path::new(&program).display()).into());
    }
    let output = Path::new(&output);
    fs::create_dir_all(output)?;
    write_font(output, Style::Regular)?;
    write_font(output, Style::Bold)?;
    Ok(())
}
