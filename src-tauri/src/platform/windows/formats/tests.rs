use super::*;

#[test]
fn text_html_and_rtf_stop_at_the_first_nul() {
    assert_eq!(effective_length(TEXT, &[65, 0, 0, 0, 99, 99]), Some(2));
    assert_eq!(effective_length(TEXT, &[65, 0]), Some(2));
    assert_eq!(effective_length(TEXT, &[65]), None);
    for name in [HTML, RTF] {
        assert_eq!(effective_length(name, b"header\nbody\0padding"), Some(11));
        assert_eq!(effective_length(name, b"body"), Some(4));
    }
}

fn png() -> Vec<u8> {
    let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
    data.extend_from_slice(&13u32.to_be_bytes());
    data.extend_from_slice(b"IHDR");
    data.extend_from_slice(&[0; 17]);
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(b"IEND");
    data.extend_from_slice(&[0; 4]);
    data
}

#[test]
fn png_ignores_padding_and_rejects_truncation_or_overflow() {
    let valid = png();
    for end in 0..valid.len() {
        assert_eq!(png_length(&valid[..end]), None);
    }
    let mut padded = valid.clone();
    padded.extend_from_slice(&[99; 20]);
    assert_eq!(png_length(&padded), Some(valid.len()));
    padded[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(png_length(&padded), None);
}

fn dib(header: usize, bits: u16, compression: u32, size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    bytes[..4].copy_from_slice(&(header as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&3i32.to_le_bytes());
    bytes[8..12].copy_from_slice(&(-2i32).to_le_bytes());
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&bits.to_le_bytes());
    bytes[16..20].copy_from_slice(&compression.to_le_bytes());
    bytes
}

#[test]
fn dib_counts_stride_palette_masks_and_rejects_truncation() {
    let rgb = dib(40, 24, 0, 64);
    assert_eq!(dib_length(&rgb), Some(64));
    for end in 0..rgb.len() {
        assert_eq!(dib_length(&rgb[..end]), None);
    }
    assert_eq!(
        dib_length(&dib(40, 8, 0, 40 + 1024 + 8 + 20)),
        Some(40 + 1024 + 8)
    );
    assert_eq!(dib_length(&dib(40, 32, 3, 40 + 12 + 24)), Some(76));
    assert_eq!(dib_length(&dib(124, 32, 3, 124 + 24 + 20)), Some(148));
    let mut overflow = rgb;
    overflow[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
    overflow[8..12].copy_from_slice(&i32::MAX.to_le_bytes());
    assert_eq!(dib_length(&overflow), None);
}

#[test]
fn dib_core_compression_and_color_profile_are_bounded() {
    let mut core = vec![0; 12 + 8];
    core[..4].copy_from_slice(&12u32.to_le_bytes());
    for (at, value) in [(4, 1u16), (6, 2), (8, 1), (10, 24)] {
        core[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    assert_eq!(dib_length(&core), Some(20));
    let mut compressed = dib(40, 8, 1, 40 + 1024 + 9);
    compressed[20..24].copy_from_slice(&9u32.to_le_bytes());
    assert_eq!(dib_length(&compressed), Some(compressed.len()));
    let mut profile = dib(124, 32, 3, 124 + 24 + 16);
    profile[112..116].copy_from_slice(&148u32.to_le_bytes());
    profile[116..120].copy_from_slice(&16u32.to_le_bytes());
    assert_eq!(dib_length(&profile), Some(164));
    profile.truncate(163);
    assert_eq!(dib_length(&profile), None);
}

#[test]
fn wide_and_ansi_dropfiles_roundtrip_without_clipboard_access() {
    let paths = vec![
        "C:\\资料\\中文😀.txt".into(),
        "D:\\sample folder\\b.txt".into(),
    ];
    let wide = build_dropfiles(&paths).unwrap();
    assert_eq!(parse_dropfiles(&wide), Some(paths));
    let mut narrow = vec![0; 20];
    narrow[..4].copy_from_slice(&20u32.to_le_bytes());
    narrow.extend_from_slice(b"C:\\sample\\a.txt\0D:\\b.txt\0\0");
    let parsed = parse_dropfiles(&narrow).unwrap();
    assert_eq!(
        parse_dropfiles(&build_dropfiles(&parsed).unwrap()),
        Some(parsed)
    );
    for end in 0..wide.len() {
        assert_eq!(parse_dropfiles(&wide[..end]), None);
    }
    narrow[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(parse_dropfiles(&narrow), None);
    assert!(build_dropfiles(&["bad\0path".into()]).is_err());
}
