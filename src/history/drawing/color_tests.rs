//! Observe colored public SVG output, theme repainting and stable tile/page boundaries.
use super::*;

/// A merge starts at a tile edge so its second parent must retain color into the following tile.
fn records() -> Vec<Commit> {
    let mut records: Vec<_> = (0..24)
        .map(|index| Commit {
            oid: index.to_string(),
            parents: (index < 23)
                .then(|| (index + 1).to_string())
                .into_iter()
                .collect(),
            ..Default::default()
        })
        .collect();
    records[15].parents = vec!["16".into(), "17".into()];
    records[16].parents = vec!["18".into()];
    records
}

/// The lower half and next tile's upper half carry the exact same branch color.
#[test]
fn colored_strands_cross_svg_tiles_and_appended_pages() {
    let records = records();
    let mut drawing = HistoryDrawing::default();
    // Column measurements share the drawing's lifetime across appended history pages.
    let mut columns = Columns::default();
    drawing.synchronize(&records[..16], &mut columns);
    let original = drawing.tile(0, 16, 56., 14., Palette::defaults(false));
    assert!(original.contains("L 24 28\" fill=\"none\" stroke=\"#c05a00\""));
    drawing.synchronize(&records, &mut columns);
    assert_eq!(
        drawing.tile(0, 16, 56., 14., Palette::defaults(false)),
        original
    );
    let next = drawing.tile(16, 16, 56., 14., Palette::defaults(false));
    assert!(next.contains("M 24 0 L 24 14\" stroke=\"#c05a00\""));
    assert!(next.contains("<circle cx=\"24\" cy=\"14\" r=\"3\" fill=\"#c05a00\""));
    assert!(next.contains("<circle cx=\"10\" cy=\"14\" r=\"3\" fill=\"#2563eb\""));
}

/// Merge rings stay empty over selection, with incident lines ending outside the transparent center.
#[test]
fn normal_nodes_are_solid_and_merge_nodes_are_hollow() {
    let mut records = records();
    records[15].parents.push("19".into());
    let mut drawing = HistoryDrawing::default();
    let mut columns = Columns::default();
    drawing.synchronize(&records, &mut columns);
    for dark in [false, true] {
        let source = drawing.tile(0, 16, 72., 14., Palette::defaults(dark));
        let groups: Vec<_> = source.split("<g transform=").skip(1).collect();
        let ordinary = groups[0];
        let merge = groups[15];
        let color = format!("#{:06x}", Palette::defaults(dark)[0]);
        assert!(ordinary.contains(&format!("r=\"3\" fill=\"{color}\"")));
        assert!(merge.contains(&format!("r=\"4\" fill=\"none\" stroke=\"{color}\"")));
        assert!(merge.contains("M 10 0 L 10 10\""));
        assert!(!merge.contains("M 10 14 L"));
        assert!(!merge.contains("L 10 14\""));
        assert!(merge.contains("M 10 18 L 10 28\""));
    }
}

/// Switching themes changes cached colors while range epochs and topology remain unchanged.
#[test]
fn theme_repaints_palette_without_resetting_history() {
    let mut state = State {
        canvas_width: 1000.,
        canvas_height: 280.,
        commits: records(),
        ..Default::default()
    };
    state.log_scroll = 14. * 28.;
    let ui::Kind::Canvas(light) = state.log_canvas().kind else {
        panic!("canvas")
    };
    let epoch = state.history_drawing.epoch;
    state.environment.dark = true;
    let ui::Kind::Canvas(dark) = state.log_canvas().kind else {
        panic!("canvas")
    };
    assert_eq!(light.scroll, dark.scroll);
    assert_eq!(state.history_drawing.epoch, epoch);
    assert_eq!(light.scroll_buffer, dark.scroll_buffer);
    let vectors = |canvas: ui::Canvas| {
        canvas
            .paint
            .into_iter()
            .filter_map(|paint| match paint {
                Paint::Svg { source, .. } => Some(source),
                _ => None,
            })
            .collect::<String>()
    };
    let light = vectors(light);
    let dark = vectors(dark);
    assert!(light.contains("stroke=\"#c05a00\""));
    assert!(dark.contains("stroke=\"#ffb05c\""));
    assert!(dark.contains("fill=\"#70a5ff\""));
    state.environment.dark = false;
    let ui::Kind::Canvas(restored) = state.log_canvas().kind else {
        panic!("canvas")
    };
    assert_eq!(vectors(restored), light);
}

/// The finite palette has no duplicates and keeps non-text graph strokes visible on both surfaces.
#[test]
fn palette_is_distinct_and_contrasts_with_theme_surfaces() {
    /// Convert packed RGB to WCAG relative luminance for a contrast assertion.
    fn luminance(rgb: u32) -> f64 {
        [16, 8, 0]
            .into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(shift, weight)| {
                let channel = ((rgb >> shift) & 255) as f64 / 255.;
                weight
                    * if channel <= 0.04045 {
                        channel / 12.92
                    } else {
                        ((channel + 0.055) / 1.055).powf(2.4)
                    }
            })
            .sum()
    }
    for (dark, background) in [(false, 0xffffff), (true, 0x202020)] {
        let mut seen = std::collections::BTreeSet::new();
        for slot in 0..graph::COLOR_COUNT {
            let color = Palette::defaults(dark)[slot];
            assert!(seen.insert(color));
            let a = luminance(color);
            let b = luminance(background);
            assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.);
        }
    }
}
