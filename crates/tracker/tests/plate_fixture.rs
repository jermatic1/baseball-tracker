//! Plate calibration on a recorded frame: a shaded plate on dark turf below
//! a batter's-box line, with brighter junk elsewhere in the frame.

use tracker::plate::{calibrate_from_frame, MAX_RMS_PX};
use tracker::Config;

#[test]
fn finds_the_plate_in_a_recorded_frame() {
    let img = image::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/plate_0264_left.png"
    ))
    .expect("fixture")
    .into_luma8();
    let (w, h) = img.dimensions();
    let cfg = Config::example();
    let (corners, fit) = calibrate_from_frame(img.as_raw(), w, h, &cfg).expect("plate");
    assert!(fit.rms_px < MAX_RMS_PX, "rms {}", fit.rms_px);
    for (x, y) in corners.px {
        assert!(
            (100.0..220.0).contains(&x) && (340.0..390.0).contains(&y),
            "{corners:?}"
        );
    }
    // Hand measurement: 6.94 ft out, 2.83 ft up, 5.67 ft to the first-base
    // side.
    let m = &fit.mount;
    assert!((m.distance - 6.94).abs() < 1.0, "{m:?}");
    assert!((m.height - 2.83).abs() < 1.0, "{m:?}");
    assert!((m.lateral - 5.67).abs() < 1.0, "{m:?}");
}
