use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::xopp;

fn sample_document() -> Document {
    let mut document = Document::new(1644.0, 924.75);
    document.layers[0].name = "底层".to_string();
    document.layers[0].strokes.push(Stroke::constant(
        Rgba::new(255, 0, 0, 255),
        2.5,
        vec![Point::new(10.0, 20.0), Point::new(30.5, 40.25)],
    ));
    document.layers[0].strokes.push(Stroke::variable(
        Rgba::new(0, 0, 255, 128),
        4.0,
        vec![1.0, 2.0, 4.0],
        vec![
            Point::new(100.0, 100.0),
            Point::new(110.0, 120.0),
            Point::new(130.0, 140.0),
        ],
    ));
    document.add_layer_above_active();
    document.layers[1].name = "上层".to_string();
    document.layers[1].strokes.push(Stroke::variable(
        Rgba::new(0, 128, 0, 255),
        6.0,
        vec![5.0, 6.0],
        vec![Point::new(200.0, 200.0), Point::new(220.0, 210.0)],
    ));
    document.active = 0;
    document
}

#[test]
fn roundtrip_preserves_layers_strokes_and_pressure_widths() {
    let document = sample_document();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.xopp");
    xopp::save(&path, &document).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "工程应为 gzip 压缩");

    let outcome = xopp::load(&path).unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert!(!outcome.ignored_unsupported);
    assert_eq!(outcome.document, document);
}

#[test]
fn xml_uses_xournal_width_token_rules() {
    let document = sample_document();
    let xml = xopp::to_xml(&document).unwrap();
    assert!(xml.contains("fileversion=\"4\""));
    assert!(xml.contains("<background type=\"solid\" color=\"#ffffffff\" style=\"plain\""));
    assert!(
        xml.contains("width=\"4 1 2\""),
        "变宽带 base + 前 N-1 个实际宽度，实际 XML：{xml}"
    );
    assert!(xml.contains("color=\"#0000ff80\""));
    assert!(xml.contains("width=\"2.5\""));
    assert!(xml.contains("tool=\"pen\""));
    assert!(xml.contains("<layer name=\"底层\">"));
    assert_eq!(xml.matches("<stroke ").count(), 3);
}

#[test]
fn plain_xml_without_gzip_is_accepted() {
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<xournal creator="test" fileversion="4">
  <page width="800" height="600">
    <background type="solid" color="#ffffffff" style="plain"/>
    <layer name="L1">
      <stroke tool="pen" color="#112233" capStyle="round" width="1">0 0 10 10</stroke>
    </layer>
  </page>
</xournal>"##;
    let outcome = xopp::from_xml(xml).unwrap();
    assert_eq!(outcome.document.width_pt, 800.0);
    assert_eq!(outcome.document.height_pt, 600.0);
    assert_eq!(outcome.document.layers.len(), 1);
    let stroke = &outcome.document.layers[0].strokes[0];
    assert_eq!(stroke.color, Rgba::new(0x11, 0x22, 0x33, 255));
    assert!(stroke.widths_pt.is_none());
    assert_eq!(stroke.points.len(), 2);
}

#[test]
fn unsupported_content_warns_and_marks_ignored() {
    let xml = r##"<xournal fileversion="4">
  <page width="800" height="600">
    <background type="pdf" filename="paper.pdf"/>
    <layer name="L">
      <stroke tool="pen" color="#000000" width="1">0 0 10 10</stroke>
      <text font="Sans">hello</text>
      <image width="10" height="10">abc</image>
    </layer>
  </page>
  <page width="800" height="600">
    <layer name="second"/>
  </page>
</xournal>"##;
    let outcome = xopp::from_xml(xml).unwrap();
    assert!(outcome.ignored_unsupported);
    assert_eq!(outcome.document.layers.len(), 1);
    assert_eq!(outcome.document.layers[0].strokes.len(), 1);
    assert!(outcome.warnings.iter().any(|w| w.contains("PDF")));
    assert!(outcome.warnings.iter().any(|w| w.contains("文本")));
    assert!(outcome.warnings.iter().any(|w| w.contains("图片")));
    assert!(outcome.warnings.iter().any(|w| w.contains("多页")));
}

#[test]
fn width_token_mismatch_falls_back_to_constant() {
    let xml = r##"<xournal fileversion="4">
  <page width="800" height="600">
    <layer name="L">
      <stroke tool="pen" color="#000000" width="2 1">0 0 10 0 20 0</stroke>
    </layer>
  </page>
</xournal>"##;
    let outcome = xopp::from_xml(xml).unwrap();
    let stroke = &outcome.document.layers[0].strokes[0];
    assert_eq!(stroke.points.len(), 3);
    assert!(stroke.widths_pt.is_none());
    assert!((stroke.base_width_pt - 2.0).abs() < 1e-9);
    assert!(outcome.warnings.iter().any(|w| w.contains("不匹配")));
}

#[test]
fn missing_xournal_root_is_rejected() {
    let error = xopp::from_xml("<page width=\"1\" height=\"1\"/>").unwrap_err();
    assert!(matches!(error, xopp::XoppError::NotXournal));
}

#[test]
fn format_number_is_compact_and_locale_independent() {
    assert_eq!(xopp::format_number(75.0), "75");
    assert_eq!(xopp::format_number(75.25), "75.25");
    assert_eq!(xopp::format_number(0.0000001), "0");
    assert_eq!(xopp::format_number(-0.0), "0");
    assert_eq!(xopp::format_number(f64::NAN), "0");
    assert_eq!(xopp::format_number(1.23456789), "1.234568");
}
