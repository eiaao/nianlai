use super::*;

fn bytes(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn text(value: &str) -> Text {
    prepare(&bytes(value)).unwrap().unwrap()
}
fn style() -> Style {
    Style::from_bits(AUTOHSCROLL)
}
fn snapshot(style: Style, length: usize, start: usize, end: usize, limit: usize) -> Snapshot {
    Snapshot::from_queries(style, length, start | (end << 16), limit, 0).unwrap()
}

#[test]
fn direct_buffer_is_exact_unicode_with_one_nul_and_no_lossy_conversion() {
    let sample = "English 中文验收 😀 <&> {\\}";
    let value = text(sample);
    assert_eq!(value.payload_units(), sample.encode_utf16().count());
    let mut expected: Vec<_> = sample.encode_utf16().collect();
    expected.push(0);
    assert_eq!(value.terminated(), expected);
    assert!(
        !format!("{value:?}").contains(sample),
        "debug metadata must not contain body"
    );
    for invalid in [&[1][..], &[0, 0xd8][..], &[0, 0, 0, 0xd8][..]] {
        assert!(matches!(prepare(invalid), Err("native_edit_invalid_text")));
    }
}

#[test]
fn known_payload_compatibility_preserves_the_original_wm_path() {
    for sample in [
        String::new(),
        "A\0B".into(),
        "A\nB".into(),
        "A\rB".into(),
        "A\r\nB\nC".into(),
        "A".repeat(MAX_UNITS + 1),
    ] {
        assert!(prepare(&bytes(&sample)).unwrap().is_none());
        assert_eq!(
            NativeTransport::wm_paste(style()).unwrap().method(),
            "wm_paste"
        );
    }
    let maximum = text(&"中".repeat(MAX_UNITS));
    assert_eq!(maximum.payload_units(), MAX_UNITS);
}

#[test]
fn direct_shape_respects_single_line_styles_and_crlf_without_normalization() {
    let single = text("中文😀");
    let crlf = text("A\r\n中😀");
    assert!(style().compatible(&single));
    assert!(!style().compatible(&crlf));
    assert!(Style::from_bits(MULTILINE).compatible(&crlf));
    for bits in [
        0,
        AUTOHSCROLL | PASSWORD,
        AUTOHSCROLL | UPPERCASE,
        AUTOHSCROLL | LOWERCASE,
        AUTOHSCROLL | OEMCONVERT,
        AUTOHSCROLL | NUMBER,
    ] {
        let observed = Style::from_bits(bits);
        assert!(!observed.compatible(&single));
        assert_eq!(
            NativeTransport::choose(text("中"), snapshot(observed, 0, 0, 0, 512))
                .unwrap()
                .method(),
            "wm_paste"
        );
    }
    assert_eq!(
        NativeTransport::choose(crlf, snapshot(style(), 0, 0, 0, 512))
            .unwrap()
            .method(),
        "wm_paste"
    );
    assert_eq!(
        NativeTransport::choose(
            text("A\r\nB"),
            snapshot(Style::from_bits(MULTILINE), 0, 0, 0, 512)
        )
        .unwrap()
        .method(),
        "em_replacesel"
    );
}

#[test]
fn initial_capacity_uses_the_current_selection_and_preserves_legacy_scope() {
    let exact = snapshot(style(), 4, 1, 3, 5);
    assert_eq!(
        NativeTransport::choose(text("中😀"), exact)
            .unwrap()
            .method(),
        "em_replacesel"
    );
    assert_eq!(
        NativeTransport::choose(text("中文😀"), exact)
            .unwrap()
            .method(),
        "wm_paste"
    );
    let full = snapshot(style(), MAX_UNITS, 0, 0, u32::MAX as usize);
    assert_eq!(
        NativeTransport::choose(text("A"), full).unwrap().method(),
        "wm_paste"
    );
    assert_eq!(
        NativeTransport::choose(text("A"), snapshot(style(), 0, 0, 0, 512))
            .unwrap()
            .method(),
        "em_replacesel"
    );
}

#[test]
fn unknown_metadata_is_an_error_not_a_legacy_transport_choice() {
    assert_eq!(legacy_length(MAX_UNITS), Ok(false));
    assert_eq!(legacy_length(MAX_UNITS + 1), Ok(true));
    assert_eq!(legacy_length(i32::MAX as usize), Ok(true));
    assert_eq!(legacy_length(i32::MAX as usize + 1), Err(QUERY_FAILED));
    assert_eq!(legacy_length(usize::MAX), Err(QUERY_FAILED));
    for (length, packed, limit, password) in [
        (MAX_UNITS + 1, 0, 512, 0),
        (4, u32::MAX as usize, 512, 0),
        (4, 3 | (2 << 16), 512, 0),
        (4, 5 << 16, 512, 0),
        (4, 0, 0, 0),
        (4, 0, 512, u16::MAX as usize + 1),
    ] {
        assert_eq!(
            Snapshot::from_queries(style(), length, packed, limit, password),
            Err(QUERY_FAILED)
        );
    }
    assert!(
        Snapshot::from_queries(style(), 0, 0, 512, 0).is_ok(),
        "acknowledged scalar zero is valid"
    );
}

#[test]
fn readonly_remains_a_common_refusal_but_password_is_only_direct_incompatibility() {
    let readonly = Style::from_bits(AUTOHSCROLL | READONLY);
    assert!(matches!(
        NativeTransport::wm_paste(readonly),
        Err("native_edit_read_only")
    ));
    assert_eq!(
        Snapshot::from_queries(readonly, 0, 0, 512, 0),
        Err("native_edit_read_only")
    );
    let mut masked = snapshot(style(), 4, 1, 3, 512);
    masked.password_character = 42;
    assert_eq!(
        NativeTransport::choose(text("中文😀"), masked)
            .unwrap()
            .method(),
        "wm_paste"
    );
}

#[test]
fn a_frozen_direct_snapshot_never_changes_into_wm_when_state_changes() {
    let expected = snapshot(style(), 4, 1, 3, 512);
    let plan = NativeTransport::choose(text("中😀"), expected).unwrap();
    assert_eq!(plan.method(), "em_replacesel");
    assert_eq!(
        NativeTransport::validate_snapshot(expected, expected),
        Ok(())
    );
    let mut changed = expected;
    changed.length += 1;
    let mut changed_selection = expected;
    changed_selection.end -= 1;
    let mut changed_limit = expected;
    changed_limit.limit -= 1;
    let mut changed_password = expected;
    changed_password.password_character = 42;
    let mut changed_style = expected;
    changed_style.style = Style::from_bits(AUTOHSCROLL | PASSWORD);
    for observed in [
        changed,
        changed_selection,
        changed_limit,
        changed_password,
        changed_style,
    ] {
        assert_eq!(
            NativeTransport::validate_snapshot(expected, observed),
            Err(STATE_CHANGED)
        );
        assert_eq!(plan.method(), "em_replacesel");
    }
    assert_eq!(
        NativeTransport::validate_style(style(), Style::from_bits(AUTOHSCROLL | READONLY)),
        Err("native_edit_read_only")
    );
}

#[test]
fn all_scalar_queries_and_the_direct_send_share_a_finite_non_reset_budget() {
    let budget = QueryBudget::new(Duration::from_millis(20));
    assert_eq!(budget.remaining_ms(Duration::from_millis(20)), Ok(200));
    assert_eq!(budget.remaining_ms(Duration::from_millis(219)), Ok(1));
    assert_eq!(
        budget.remaining_ms(Duration::from_millis(220)),
        Err(QUERY_TIMEOUT)
    );
    assert_eq!(
        budget.remaining_ms(Duration::from_millis(250)),
        Err(QUERY_TIMEOUT)
    );
}
