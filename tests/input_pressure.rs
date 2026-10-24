use link_ink::input::{clamp_pressure, pressure_width};

#[test]
fn missing_pressure_falls_back_to_full() {
    assert_eq!(clamp_pressure(None), (1.0, false));
    assert_eq!(clamp_pressure(Some(f64::NAN)), (1.0, false));
    assert_eq!(clamp_pressure(Some(f64::INFINITY)), (1.0, false));
}

#[test]
fn out_of_range_pressure_is_clamped() {
    assert_eq!(clamp_pressure(Some(-0.5)), (0.0, true));
    assert_eq!(clamp_pressure(Some(1.5)), (1.0, true));
    assert_eq!(clamp_pressure(Some(0.42)), (0.42, true));
}

#[test]
fn width_grows_with_pressure() {
    let low = pressure_width(4.0, 0.1, 1.0, 0.25);
    let mid = pressure_width(4.0, 0.5, 1.0, 0.25);
    let high = pressure_width(4.0, 1.0, 1.0, 0.25);
    assert!(low < mid && mid < high);
    assert!((low - 4.0 * (0.25 + 0.75 * 0.1)).abs() < 1e-9);
    assert!((high - 4.0).abs() < 1e-9);
}

#[test]
fn min_ratio_keeps_light_strokes_visible() {
    let light = pressure_width(10.0, 0.0, 1.0, 0.3);
    assert!((light - 3.0).abs() < 1e-9);
    let constant = pressure_width(10.0, 0.3, 1.0, 1.0);
    assert!((constant - 10.0).abs() < 1e-9);
}

#[test]
fn degenerate_parameters_are_sanitized() {
    let baseline = pressure_width(2.0, 0.5, 1.0, 0.2);
    assert!((pressure_width(2.0, 0.5, 0.0, 0.2) - baseline).abs() < 1e-9);
    assert!((pressure_width(2.0, 0.5, f64::NAN, 0.2) - baseline).abs() < 1e-9);
    let clamped_ratio = pressure_width(2.0, 0.5, 1.0, 5.0);
    assert!((clamped_ratio - 2.0).abs() < 1e-9);
    let nan_pressure = pressure_width(2.0, f64::NAN, 1.0, 0.2);
    assert!((nan_pressure - 2.0).abs() < 1e-9);
}
