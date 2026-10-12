# Protocol

## Socket

Simulators connect to `ws://<tracker>:7880/ws`. Messages are JSON with a `type` field; preview frames are binary JPEGs.

Client to tracker: `hello {client}` first, then `preview {enabled}`, `set {exposure_us, gain, ir_flood, ir_dot}` with any subset, `record {action: start|stop}`, and `calibrate {action: start|apply|stop}`.

Tracker to clients: `status` twice a second (camera and detector rates, connected clients, whether capturing, calibrating, or recording, current controls and mount), `event {clip, record}` for each hit, pitch, or bounce, `calibration {found, corners, reprojected, rms_px, mount, applied}` while calibrating, and `error {message}` to the sender of a bad command.

An event record carries `kind`, `hit_type`, `exit_velocity_mph`, `launch_angle_deg`, `spray_angle_deg`, `pitch_mph` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `t_start_ns`, `samples`, and `confident`. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.
