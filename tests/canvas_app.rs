use link_ink::canvas_app::{CanvasApp, CanvasHost};
use link_ink::platform::{CaptureImage, MonitorInfo, PlatformError, RectF, RectI};
use link_ipc::{BackgroundDto, Command, Event, ImageFormatDto, PROTOCOL_VERSION};

struct MockHost {
    passthrough: bool,
}

impl CanvasHost for MockHost {
    fn set_click_through(&mut self, enabled: bool) -> Result<(), PlatformError> {
        self.passthrough = enabled;
        Ok(())
    }

    fn capture_primary_hidden(&mut self) -> Result<CaptureImage, PlatformError> {
        Err(PlatformError::Capture("测试环境没有屏幕".to_string()))
    }
}

fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 150.0,
        },
        scale: 2.0,
    }
}

fn setup() -> (CanvasApp, MockHost) {
    (CanvasApp::new(monitor()), MockHost { passthrough: false })
}

fn state_layers(event: &Event) -> Vec<(String, bool)> {
    match event {
        Event::State { layers, .. } => layers
            .iter()
            .map(|layer| (layer.name.clone(), layer.visible))
            .collect(),
        other => panic!("期望 State 事件，实际 {other:?}"),
    }
}

#[test]
fn hello_replies_ready_and_state_with_thumbnails() {
    let (mut canvas, mut host) = setup();
    let events = canvas.handle_command(
        Command::Hello {
            proto: PROTOCOL_VERSION,
        },
        &mut host,
    );

    let ready = events.first().expect("应有 Ready");
    match ready {
        Event::Ready { proto, monitor } => {
            assert_eq!(*proto, PROTOCOL_VERSION);
            assert_eq!(monitor.width, 400);
            assert_eq!(monitor.scale, 2.0);
        }
        other => panic!("首个事件应为 Ready，实际 {other:?}"),
    }

    let state = events.last().expect("应有 State");
    assert_eq!(state_layers(state), vec![("图层 1".to_string(), true)]);
    match state {
        Event::State { layers, active, .. } => {
            assert_eq!(*active, 0);
            assert!(
                layers[0]
                    .thumbnail_png
                    .as_deref()
                    .is_some_and(|png| !png.is_empty()),
                "首次状态应带缩略图"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn protocol_mismatch_reports_error_without_panicking() {
    let (mut canvas, mut host) = setup();
    let events = canvas.handle_command(Command::Hello { proto: 999 }, &mut host);
    assert!(matches!(events.as_slice(), [Event::Error { .. }]));
}

#[test]
fn layer_commands_roundtrip_through_state_events() {
    let (mut canvas, mut host) = setup();

    let events = canvas.handle_command(Command::LayerAdd, &mut host);
    assert_eq!(state_layers(&events[0]).len(), 2);

    let events = canvas.handle_command(
        Command::LayerRename {
            index: 1,
            name: "草稿".to_string(),
        },
        &mut host,
    );
    assert_eq!(state_layers(&events[0])[1].0, "草稿");

    let events = canvas.handle_command(
        Command::LayerVisibility {
            index: 0,
            visible: false,
        },
        &mut host,
    );
    assert!(!state_layers(&events[0])[0].1);

    let events = canvas.handle_command(Command::LayerSelect { index: 0 }, &mut host);
    match &events[0] {
        Event::State { active, .. } => assert_eq!(*active, 0),
        other => panic!("期望 State，实际 {other:?}"),
    }

    let events = canvas.handle_command(Command::LayerRemove, &mut host);
    assert_eq!(state_layers(&events[0]).len(), 1);
}

#[test]
fn passthrough_is_forwarded_to_host_and_reflected_in_state() {
    let (mut canvas, mut host) = setup();
    let events = canvas.handle_command(Command::SetPassthrough { enabled: true }, &mut host);
    assert!(host.passthrough, "应把穿透交给 host 执行");
    match &events[0] {
        Event::State { passthrough, .. } => assert!(*passthrough),
        other => panic!("期望 State，实际 {other:?}"),
    }
    assert!(canvas.passthrough());
}

#[test]
fn blank_export_writes_a_png() {
    let (mut canvas, mut host) = setup();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blank.png");

    let events = canvas.handle_command(
        Command::ExportImage {
            path: path.display().to_string(),
            format: ImageFormatDto::Png,
            background: BackgroundDto::Blank,
        },
        &mut host,
    );
    match &events[0] {
        Event::Exported { path: reported } => assert_eq!(reported, &path.display().to_string()),
        other => panic!("期望 Exported，实际 {other:?}"),
    }
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], &[0x89, 0x50], "应写出 PNG");
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!(image.width(), 400);
    assert_eq!(image.height(), 300);
}

#[test]
fn screen_export_without_capture_reports_error() {
    let (mut canvas, mut host) = setup();
    let dir = tempfile::tempdir().unwrap();
    let events = canvas.handle_command(
        Command::ExportImage {
            path: dir.path().join("screen.png").display().to_string(),
            format: ImageFormatDto::Png,
            background: BackgroundDto::Screen,
        },
        &mut host,
    );
    assert!(matches!(events.as_slice(), [Event::Error { .. }]));
}

#[test]
fn save_and_reload_xopp_through_ipc_commands() {
    let (mut canvas, mut host) = setup();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.xopp");

    let events = canvas.handle_command(
        Command::SaveXopp {
            path: path.display().to_string(),
        },
        &mut host,
    );
    assert!(matches!(events.as_slice(), [Event::Saved { .. }]));
    assert!(path.exists());

    let events = canvas.handle_command(
        Command::OpenXopp {
            path: path.display().to_string(),
        },
        &mut host,
    );
    assert!(matches!(events.first(), Some(Event::Opened { .. })));
    assert!(matches!(events.last(), Some(Event::State { .. })));
}
