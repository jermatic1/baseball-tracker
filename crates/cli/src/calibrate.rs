use std::path::PathBuf;

#[cfg(not(feature = "oak"))]
pub fn calibrate(_path: PathBuf) -> Result<(), String> {
    Err("rebuild with --features oak".into())
}

/// Grab one frame with the plate clear, solve the camera pose from it, and
/// write the mount section of the session config.
#[cfg(feature = "oak")]
pub fn calibrate(path: PathBuf) -> Result<(), String> {
    use device::Camera;
    use std::time::{Duration, Instant};

    let mut session = tracker::Session::create(&path).map_err(|e| e.to_string())?;
    let c = &session.config.capture;
    let mut cam = device::OakCamera::open(c.width, c.height, c.fps as f32, c.exposure_us, c.gain)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let frame = loop {
        if Instant::now() > deadline {
            return Err("no frame from camera".into());
        }
        match cam
            .poll(Duration::from_millis(0))
            .map_err(|e| e.to_string())?
        {
            Some(frame) => break frame,
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let (corners, fit) = tracker::plate::calibrate_from_frame(
        &frame.left,
        frame.width,
        frame.height,
        &session.config,
    )
    .ok_or("plate not found: clear the plate, keep the ball off it, and retry")?;
    let mut marked = frame.left.clone();
    draw_polygon(&mut marked, frame.width, frame.height, &corners.px, 255);
    draw_polygon(
        &mut marked,
        frame.width,
        frame.height,
        &fit.reprojected,
        160,
    );
    let jpeg = crate::jpeg::encode_gray_jpeg(frame.width, frame.height, &marked)?;
    std::fs::write(session.dir.join("calibration.jpg"), jpeg).map_err(|e| e.to_string())?;
    session.config.mount = fit.mount.clone();
    session
        .config
        .save(session.dir.join("config.toml"))
        .map_err(|e| e.to_string())?;
    print_fit(&fit);
    println!(
        "wrote [mount] to {}",
        session.dir.join("config.toml").display()
    );
    Ok(())
}

pub fn print_fit(fit: &tracker::plate::PoseFit) {
    let m = &fit.mount;
    println!(
        "plate fit rms {:.2} px: distance {:.2} m, height {:.2} m, lateral {:.2} m, yaw {:.1} deg, pitch {:.1} deg",
        fit.rms_px, m.distance_from_plate_m, m.height_m, m.lateral_offset_m, m.yaw_deg, m.pitch_deg
    );
}

/// Bresenham lines joining consecutive points, closed.
#[cfg(feature = "oak")]
pub fn draw_polygon(img: &mut [u8], w: u32, h: u32, pts: &[(f64, f64)], value: u8) {
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let (mut x0, mut y0) = (a.0.round() as i64, a.1.round() as i64);
        let (x1, y1) = (b.0.round() as i64, b.1.round() as i64);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        loop {
            if x0 >= 0 && y0 >= 0 && (x0 as u32) < w && (y0 as u32) < h {
                img[(y0 as u32 * w + x0 as u32) as usize] = value;
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
}
