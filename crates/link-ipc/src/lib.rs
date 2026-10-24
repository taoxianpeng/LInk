//! LInk 画布进程 ↔ 面板 之间的 IPC 协议。
//!
//! 纯 DTO，不含任何 GUI 依赖：面板（egui）和画布（GTK）都只依赖这个 crate，
//! 协议就不会各写一份而漂移。
//!
//! 传输格式：**一行一个 JSON**（stdin/stdout），首行握手见 [`Command::Hello`]。

use std::io::{BufRead, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// 协议版本。面板与画布不一致时画布返回 [`Event::Error`] 而不是崩溃。
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDto {
    Pen,
    Eraser,
    Line,
    Rect,
    Ellipse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormatDto {
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundDto {
    /// 白底，只保留笔迹。
    Blank,
    /// 真实屏幕背景。
    Screen,
}

/// 面板 → 画布。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// 握手。画布收到后回 [`Event::Ready`]。
    Hello {
        proto: u32,
    },
    SetTool {
        tool: ToolDto,
    },
    SetColor {
        rgba: [u8; 4],
    },
    SetWidth {
        pt: f64,
    },
    SetPressure {
        gamma: f64,
        min_ratio: f64,
    },
    LayerAdd,
    LayerRemove,
    LayerMove {
        up: bool,
    },
    LayerSelect {
        index: usize,
    },
    LayerRename {
        index: usize,
        name: String,
    },
    LayerVisibility {
        index: usize,
        visible: bool,
    },
    ClearLayer,
    SetPassthrough {
        enabled: bool,
    },
    SaveXopp {
        path: String,
    },
    OpenXopp {
        path: String,
    },
    ExportImage {
        path: String,
        format: ImageFormatDto,
        background: BackgroundDto,
    },
    Quit,
}

/// 画布 → 面板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "evt", rename_all = "snake_case")]
pub enum Event {
    Ready {
        proto: u32,
        monitor: MonitorDto,
    },
    State {
        layers: Vec<LayerView>,
        active: usize,
        passthrough: bool,
    },
    Device {
        name: String,
        tool: String,
        pressure: f64,
        has_pressure: bool,
    },
    Exported {
        path: String,
    },
    Saved {
        path: String,
    },
    Opened {
        path: String,
        warnings: Vec<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonitorDto {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub logical_width: f64,
    pub logical_height: f64,
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerView {
    pub id: usize,
    pub name: String,
    pub visible: bool,
    /// 图层缩略图（PNG 的 base64）。内容没变时可以省略，面板按 id 复用旧纹理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_png: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("读写失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("消息无法解析: {0}")]
    Decode(String),
}

fn encode<T: Serialize>(value: &T) -> Result<String, IpcError> {
    serde_json::to_string(value).map_err(|error| IpcError::Decode(error.to_string()))
}

fn decode<T: DeserializeOwned>(line: &str) -> Result<T, IpcError> {
    serde_json::from_str(line).map_err(|error| IpcError::Decode(error.to_string()))
}

/// 读一行并解析；EOF 返回 `Ok(None)`。空行会被跳过。
pub fn read_json_line<R: BufRead, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>, IpcError> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return decode(trimmed).map(Some);
    }
}

/// 写一行 JSON 并刷新（管道需要立即送达）。
pub fn write_json_line<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), IpcError> {
    let mut line = encode(value)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

pub fn read_command<R: BufRead>(reader: &mut R) -> Result<Option<Command>, IpcError> {
    read_json_line(reader)
}

pub fn read_event<R: BufRead>(reader: &mut R) -> Result<Option<Event>, IpcError> {
    read_json_line(reader)
}

pub fn write_command<W: Write>(writer: &mut W, command: &Command) -> Result<(), IpcError> {
    write_json_line(writer, command)
}

pub fn write_event<W: Write>(writer: &mut W, event: &Event) -> Result<(), IpcError> {
    write_json_line(writer, event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_roundtrip_through_json_lines() {
        let commands = vec![
            Command::Hello {
                proto: PROTOCOL_VERSION,
            },
            Command::SetTool {
                tool: ToolDto::Ellipse,
            },
            Command::SetColor {
                rgba: [12, 34, 56, 255],
            },
            Command::LayerMove { up: true },
            Command::ExportImage {
                path: "/tmp/a.png".to_string(),
                format: ImageFormatDto::Jpeg,
                background: BackgroundDto::Screen,
            },
        ];
        for command in commands {
            let mut buffer = Vec::new();
            write_command(&mut buffer, &command).unwrap();
            assert!(buffer.ends_with(b"\n"));
            let mut cursor = std::io::Cursor::new(buffer);
            let decoded = read_command(&mut cursor).unwrap().unwrap();
            assert_eq!(decoded, command);
        }
    }

    #[test]
    fn events_roundtrip_and_eof_is_none() {
        let event = Event::State {
            layers: vec![LayerView {
                id: 3,
                name: "图层 1".to_string(),
                visible: true,
                thumbnail_png: Some("AAAA".to_string()),
            }],
            active: 3,
            passthrough: false,
        };
        let mut buffer = Vec::new();
        write_event(&mut buffer, &event).unwrap();
        let mut cursor = std::io::Cursor::new(buffer);
        assert_eq!(read_event(&mut cursor).unwrap().unwrap(), event);
        assert_eq!(read_event(&mut cursor).unwrap(), None);
    }

    #[test]
    fn unknown_command_is_a_decode_error_not_a_panic() {
        let mut cursor = std::io::Cursor::new(b"{\"cmd\":\"spawn_dragon\"}\n".to_vec());
        let error = read_command(&mut cursor).unwrap_err();
        assert!(matches!(error, IpcError::Decode(_)));
    }
}
