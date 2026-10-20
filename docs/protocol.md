# Protocol

## Socket

Simulators connect to `ws://<tracker>:7880/ws`. Messages are JSON with a `type` field; preview frames are binary JPEGs.

To find the tracker, broadcast `{"type": "discover", "version": 1}` to UDP port 7881 on the local network. The tracker answers the sender with `{"type": "tracker", "version": 1, "port": 7880, "path": "/ws", "name": "<hostname>"}`; the address is the reply's source IP. If the tracker machine runs a firewall, allow TCP 7880 and UDP 7881 in.

Client to tracker: `hello {client}` first, then `preview {enabled}`, `set {exposure_us, gain, ir_flood, ir_dot}` with any subset, `record {action: start|stop}`, and `calibrate {action: start|apply|stop}`.

Tracker to clients: `status` twice a second (camera and detector rates, connected clients, whether capturing, calibrating, or recording, current controls and mount), `event {clip, record}` for each hit, pitch, or bounce, `calibration {found, corners, reprojected, rms_px, mount, applied}` while calibrating, and `error {message}` to the sender of a bad command.

An event record carries `kind`, `hit_type`, `exit_velocity_mph`, `launch_angle_deg`, `spray_angle_deg`, `pitch_mph` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `t_start_ns`, `samples`, and `confident`. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.
