# Protocol

## Socket

Simulators connect to `ws://<tracker>:7880/ws`. Messages are JSON with a `type` field; preview frames are binary JPEGs. Feet, inches, mph, degrees, and seconds throughout.

To find the tracker, broadcast `{"type": "discover", "version": 1}` to UDP port 7881 on the local network. The tracker answers the sender with `{"type": "tracker", "version": 1, "port": 7880, "path": "/ws", "name": "<hostname>"}`; the address is the reply's source IP. If the tracker machine runs a firewall, allow TCP 7880 and UDP 7881 in.

Client to tracker: `hello {client, games}` first, then `preview {enabled}`, `set {exposure, gain, ir_flood, ir_dot}` with any subset, `record {action: start|stop}`, `calibrate {action: start|apply|stop}`, `add_user {name}`, `start_session {user, activity: hitting|pitching, mode: game|practice}`, and `end_session {session}`. Unknown message types are ignored.

Tracker to clients: `users {users: [{id, name}]}` and `session {session: null | {id, user, activity, mode}}` after hello and whenever they change, `session_summary {session, user, lines: [{label, value}]}` when a session ends and again after hello for the most recent one, `status` twice a second (camera and detector rates, connected clients, whether capturing, calibrating, or recording, current controls and mount), `event {clip, record}` for each hit, pitch, or bounce, `calibration {found, corners, reprojected, rms, mount, applied}` while calibrating, and `error {request, message}` to the sender of a bad command.

Starting a session while one is active ends it: the old session's summary is sent, then the new session. Ending one sends the null session, then its summary.

An event record carries `kind`, `hit_type`, `exit_velocity`, `launch_angle`, `spray_angle`, `pitch_velocity` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `start`, `samples`, and `confident`. A hit adds `hit`: the predicted `carry`, `hang_time`, `peak_height`, `landing`, and a `path` of about ten points a second for animation. A pitch adds `pitch`: `plate_velocity`, its `location` at the front of the plate, `simulated_location` for the same throw from the configured mound distance, and the `call` (`ball` or `strike`) made on that simulated crossing against the `zone` it was judged by. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.
