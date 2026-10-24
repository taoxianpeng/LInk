use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use quick_xml::events::{BytesDecl, BytesStart, BytesText, Event};
use quick_xml::Writer;

use crate::model::{Document, Layer, Point, Rgba, Stroke};

pub const CREATOR: &str = "LInk 0.1";
pub const FILE_VERSION: &str = "4";
const DEFAULT_PAGE_WIDTH_PT: f64 = 800.0;
const DEFAULT_PAGE_HEIGHT_PT: f64 = 600.0;

#[derive(Debug, thiserror::Error)]
pub enum XoppError {
    #[error("读写文件失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("XML 解析失败：{0}")]
    Xml(#[from] quick_xml::Error),
    #[error("工程文件不是有效的 UTF-8 文本：{0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("工程文件缺少 <xournal> 根元素")]
    NotXournal,
    #[error("工程文件没有页面")]
    NoPage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadOutcome {
    pub document: Document,
    pub warnings: Vec<String>,
    pub ignored_unsupported: bool,
}

pub fn save(path: &Path, document: &Document) -> Result<(), XoppError> {
    fs::write(path, to_bytes(document)?)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<LoadOutcome, XoppError> {
    from_bytes(&fs::read(path)?)
}

pub fn to_bytes(document: &Document) -> Result<Vec<u8>, XoppError> {
    let xml = to_xml(document)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(xml.as_bytes())?;
    Ok(encoder.finish()?)
}

pub fn from_bytes(bytes: &[u8]) -> Result<LoadOutcome, XoppError> {
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(bytes);
        let mut text = String::new();
        decoder.read_to_string(&mut text)?;
        text
    } else {
        String::from_utf8(bytes.to_vec())?
    };
    from_xml(&text)
}

pub fn to_xml(document: &Document) -> Result<String, XoppError> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
    writer.write_event(Event::Decl(BytesDecl::new(
        "1.0",
        Some("UTF-8"),
        Some("no"),
    )))?;

    let width = format_number(document.width_pt);
    let height = format_number(document.height_pt);
    writer
        .create_element("xournal")
        .with_attribute(("creator", CREATOR))
        .with_attribute(("fileversion", FILE_VERSION))
        .write_inner_content(|writer| {
            writer
                .create_element("title")
                .write_text_content(BytesText::new("LInk document"))?;
            writer
                .create_element("page")
                .with_attribute(("width", width.as_str()))
                .with_attribute(("height", height.as_str()))
                .write_inner_content(|writer| {
                    writer
                        .create_element("background")
                        .with_attribute(("type", "solid"))
                        .with_attribute(("color", "#ffffffff"))
                        .with_attribute(("style", "plain"))
                        .write_empty()?;
                    if document.layers.is_empty() {
                        writer
                            .create_element("layer")
                            .with_attribute(("name", "图层 1"))
                            .write_inner_content(|writer| write_layer(writer, &[]))?;
                    } else {
                        for layer in &document.layers {
                            writer
                                .create_element("layer")
                                .with_attribute(("name", layer.name.as_str()))
                                .write_inner_content(|writer| {
                                    write_layer(writer, &layer.strokes)
                                })?;
                        }
                    }
                    Ok(())
                })?;
            Ok(())
        })?;

    let bytes = writer.into_inner();
    Ok(String::from_utf8(bytes).expect("写入器只产生 ASCII/UTF-8"))
}

fn write_layer<W: Write>(writer: &mut Writer<W>, strokes: &[Stroke]) -> std::io::Result<()> {
    for stroke in strokes {
        write_stroke(writer, stroke)?;
    }
    Ok(())
}

fn write_stroke<W: Write>(writer: &mut Writer<W>, stroke: &Stroke) -> std::io::Result<()> {
    if stroke.points.is_empty() {
        return Ok(());
    }
    let color = stroke.color.to_hex();
    let width = stroke_width_attr(stroke);
    let mut coordinates = String::with_capacity(stroke.points.len() * 20);
    for (index, point) in stroke.points.iter().enumerate() {
        if index > 0 {
            coordinates.push(' ');
        }
        coordinates.push_str(&format_number(point.x));
        coordinates.push(' ');
        coordinates.push_str(&format_number(point.y));
    }
    writer
        .create_element("stroke")
        .with_attribute(("tool", "pen"))
        .with_attribute(("color", color.as_str()))
        .with_attribute(("capStyle", "round"))
        .with_attribute(("width", width.as_str()))
        .write_text_content(BytesText::new(&coordinates))?;
    Ok(())
}

fn stroke_width_attr(stroke: &Stroke) -> String {
    let mut text = format_number(stroke.base_width_pt);
    let Some(widths) = &stroke.widths_pt else {
        return text;
    };
    if widths.len() != stroke.points.len() || widths.len() < 2 {
        return text;
    }
    for width in &widths[..widths.len() - 1] {
        text.push(' ');
        text.push_str(&format_number(*width));
    }
    text
}

pub fn format_number(value: f64) -> String {
    let value = if value.is_finite() {
        if value == 0.0 {
            0.0
        } else {
            value
        }
    } else {
        0.0
    };
    let mut text = format!("{value:.6}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        text = "0".to_string();
    }
    text
}

pub fn from_xml(text: &str) -> Result<LoadOutcome, XoppError> {
    let mut reader = quick_xml::Reader::from_str(text.trim_start_matches('\u{feff}'));
    reader.config_mut().trim_text(true);

    let mut saw_root = false;
    let mut page_count = 0usize;
    let mut on_first_page = false;
    let mut page_width = None;
    let mut page_height = None;
    let mut layers: Vec<Layer> = Vec::new();
    let mut current_layer: Option<Layer> = None;
    let mut builder: Option<StrokeBuilder> = None;
    let mut stroke_text = String::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut ignored_unsupported = false;
    let mut warned_text = false;
    let mut warned_image = false;
    let mut warned_pdf = false;

    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(element) => {
                let name = element.name();
                match name.as_ref() {
                    "xournal" => saw_root = true,
                    "page" => {
                        page_count += 1;
                        if page_count == 1 {
                            on_first_page = true;
                            page_width = attr_f64(&element, "width");
                            page_height = attr_f64(&element, "height");
                        } else {
                            on_first_page = false;
                            ignored_unsupported = true;
                            if page_count == 2 {
                                warnings.push(
                                    "工程包含多页，仅加载第一页；请另存后再编辑，避免覆盖原文件。"
                                        .to_string(),
                                );
                            }
                        }
                    }
                    "layer" if on_first_page => {
                        let name = attr_string(&element, "name")
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| format!("图层 {}", layers.len() + 1));
                        current_layer = Some(Layer::new(name));
                    }
                    "stroke" if on_first_page && current_layer.is_some() => {
                        builder = Some(StrokeBuilder::from_element(&element, &mut warnings));
                        stroke_text.clear();
                    }
                    "background" if on_first_page => check_background(
                        &element,
                        &mut warnings,
                        &mut ignored_unsupported,
                        &mut warned_pdf,
                    ),
                    "text" if on_first_page => {
                        ignored_unsupported = true;
                        if !warned_text {
                            warnings.push("工程包含文本元素，暂不支持，已跳过。".to_string());
                            warned_text = true;
                        }
                    }
                    "image" if on_first_page => {
                        ignored_unsupported = true;
                        if !warned_image {
                            warnings.push("工程包含图片元素，暂不支持，已跳过。".to_string());
                            warned_image = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => {
                let name = element.name();
                match name.as_ref() {
                    "background" if on_first_page => check_background(
                        &element,
                        &mut warnings,
                        &mut ignored_unsupported,
                        &mut warned_pdf,
                    ),
                    "text" if on_first_page => {
                        ignored_unsupported = true;
                        if !warned_text {
                            warnings.push("工程包含文本元素，暂不支持，已跳过。".to_string());
                            warned_text = true;
                        }
                    }
                    "image" if on_first_page => {
                        ignored_unsupported = true;
                        if !warned_image {
                            warnings.push("工程包含图片元素，暂不支持，已跳过。".to_string());
                            warned_image = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(text) if builder.is_some() => {
                let raw = text.into_inner();
                match quick_xml::escape::unescape(raw.as_ref()) {
                    Ok(decoded) => stroke_text.push_str(&decoded),
                    Err(_) => warnings.push("笔迹坐标包含无法解码的文本，已跳过。".to_string()),
                }
            }
            Event::End(element) => match element.name().as_ref() {
                "stroke" => {
                    if let (Some(builder), Some(layer)) = (builder.take(), current_layer.as_mut()) {
                        if let Some(stroke) = builder.finish(&stroke_text, &mut warnings) {
                            layer.strokes.push(stroke);
                        }
                    }
                    stroke_text.clear();
                }
                "layer" => {
                    if let Some(layer) = current_layer.take() {
                        layers.push(layer);
                    }
                }
                "page" if page_count == 1 => on_first_page = false,
                _ => {}
            },
            _ => {}
        }
    }

    if !saw_root {
        return Err(XoppError::NotXournal);
    }
    if page_count == 0 {
        return Err(XoppError::NoPage);
    }
    if page_width.is_none() || page_height.is_none() {
        warnings.push("页面尺寸缺失，已使用默认页面尺寸。".to_string());
    }
    if layers.is_empty() {
        layers.push(Layer::new("图层 1"));
    }
    let mut document = Document::new(
        page_width.unwrap_or(DEFAULT_PAGE_WIDTH_PT),
        page_height.unwrap_or(DEFAULT_PAGE_HEIGHT_PT),
    );
    document.layers = layers;
    document.active = 0;

    Ok(LoadOutcome {
        document,
        warnings,
        ignored_unsupported,
    })
}

struct StrokeBuilder {
    color: Rgba,
    base_width: f64,
    width_tokens: Vec<f64>,
}

impl StrokeBuilder {
    fn from_element(element: &BytesStart<'_>, warnings: &mut Vec<String>) -> Self {
        let color = attr_string(element, "color")
            .and_then(|text| Rgba::from_hex(&text))
            .unwrap_or_else(|| {
                warnings.push("笔迹颜色无效，已按黑色加载。".to_string());
                Rgba::BLACK
            });
        let raw_width = attr_string(element, "width");
        let mut width_tokens: Vec<f64> = Vec::new();
        let mut invalid_width = false;
        if let Some(text) = &raw_width {
            for token in text.split_whitespace() {
                match token.parse::<f64>() {
                    Ok(value) if value.is_finite() && value > 0.0 => width_tokens.push(value),
                    _ => invalid_width = true,
                }
            }
        }
        let base_width = if let Some(base) = width_tokens.first().copied() {
            if invalid_width {
                warnings.push("笔迹宽度包含无效数值，已按可解析的宽度加载。".to_string());
            }
            base
        } else {
            warnings.push(if raw_width.is_some() {
                "笔迹线宽无效，已按 1 pt 加载。".to_string()
            } else {
                "笔迹缺少线宽，已按 1 pt 加载。".to_string()
            });
            1.0
        };
        Self {
            color,
            base_width,
            width_tokens,
        }
    }

    fn finish(&self, text: &str, warnings: &mut Vec<String>) -> Option<Stroke> {
        let mut values = Vec::new();
        for token in text.split_whitespace() {
            match token.parse::<f64>() {
                Ok(value) if value.is_finite() => values.push(value),
                _ => {
                    warnings.push("笔迹坐标包含无效数字，已跳过该笔迹。".to_string());
                    return None;
                }
            }
        }
        if values.len() % 2 != 0 {
            warnings.push("笔迹坐标数量为奇数，已忽略最后一个数值。".to_string());
            values.pop();
        }
        if values.len() < 2 {
            return None;
        }
        let mut points: Vec<Point> = values
            .chunks_exact(2)
            .map(|pair| Point::new(pair[0], pair[1]))
            .collect();
        if points.len() == 1 {
            points.push(points[0]);
        }
        let widths = if self.width_tokens.len() == 1 {
            None
        } else if self.width_tokens.len() == points.len() {
            let last = points.len() - 1;
            let mut widths = Vec::with_capacity(points.len());
            for index in 0..last {
                widths.push(self.width_tokens[index + 1]);
            }
            widths.push(self.base_width);
            Some(widths)
        } else {
            warnings.push(format!(
                "笔迹宽度数量 {} 与点数 {} 不匹配，已按常量宽度加载。",
                self.width_tokens.len(),
                points.len()
            ));
            None
        };
        Some(match widths {
            Some(widths) => Stroke::variable(self.color, self.base_width, widths, points),
            None => Stroke::constant(self.color, self.base_width, points),
        })
    }
}

fn check_background(
    element: &BytesStart<'_>,
    warnings: &mut Vec<String>,
    ignored_unsupported: &mut bool,
    warned_pdf: &mut bool,
) {
    let kind = attr_string(element, "type").unwrap_or_default();
    match kind.as_str() {
        "solid" => {}
        "pdf" => {
            *ignored_unsupported = true;
            if !*warned_pdf {
                warnings.push("工程使用 PDF 背景，暂不支持，已按空白背景加载。".to_string());
                *warned_pdf = true;
            }
        }
        other => {
            *ignored_unsupported = true;
            warnings.push(format!("背景类型“{other}”不受支持，已按空白背景加载。"));
        }
    }
}

fn attr_string(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| {
            attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
        })
        .map(|value| value.into_owned())
}

fn attr_f64(element: &BytesStart<'_>, key: &str) -> Option<f64> {
    attr_string(element, key)
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
}
