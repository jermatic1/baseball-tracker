# Protocol

## Socket

Simulators connect to `ws://<tracker>:7880/ws`. Messages are JSON with a `type` field; preview frames are binary JPEGs. Feet, inches, mph, degrees, and seconds throughout.

To find the tracker, broadcast `{"type": "discover", "version": 1}` to UDP port 7881 on the local network. The tracker answers the sender with `{"type": "tracker", "version": 1, "port": 7880, "path": "/ws", "name": "<hostname>"}`; the address is the reply's source IP. If the tracker machine runs a firewall, allow TCP 7880 and UDP 7881 in.

Client to tracker: `hello {client, games}` first (a `client` of `page` or ending in ` page` is one of the tracker's pages: it does not start the camera and hears every game), then `preview {enabled}`, `set {exposure, gain, ir_flood, ir_dot}` with any subset, `record {action: start|stop}`, `calibrate {action: start|apply|stop}` or `calibrate {action: corners, corners: [[x, y] x5]}` with clicked plate corners in frame pixels, which fits the pose for Apply, `add_user {name}`, `start_session {user, activity: hitting|pitching, mode: game|practice}`, `end_session {session}`, and `game {game, body}` for a game module. Unknown message types are ignored.

Tracker to clients: `users {users: [{id, name}]}` and `session {session: null | {id, user, activity, mode}}` after hello and whenever they change, `session_stats {session, pitches, strikes, avg_pitch_mph, top_pitch_mph, avg_exit_mph, top_exit_mph, longest_ft}` when a session starts, after each of its events, and after hello while one is active, `session_summary {session, user, lines: [{label, value}]}` when a session ends and again after hello for the most recent one, `status` twice a second (camera and detector rates, connected clients, whether capturing, calibrating, or recording, current controls and mount, and the `model` with its file, short sha1, when it was promoted, how many reviewed sessions marked include it has not trained on, whether a promoted model is loading or failed to; the file is empty when there is no detector yet), `event {clip, record}` for each hit, pitch, or bounce, `recorded {session, clip}` when a clip recorded by hand is saved (`session` is `none` for the data directory itself), `calibration {found, corners, reprojected, rms, mount, applied, source: auto|clicked, warnings}` while calibrating, with `warnings` naming what looks wrong with the fit, `game {game, body}` from a module to the clients that listed it, and `error {request, message}` to the sender of a bad command.

Starting a session while one is active ends it: the old session's summary is sent, then the new session. Ending one sends the null session, then its summary.

An event record carries `kind`, `hit_type`, `exit_velocity`, `launch_angle`, `spray_angle`, `pitch_velocity` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `start`, `samples`, and `confident`. A hit adds `hit`: the predicted `carry`, `hang_time`, `peak_height`, `landing`, and a `path` of about ten points a second for animation. A pitch adds `pitch`: `plate_velocity`, its `location` at the front of the plate, `simulated_location` for the same throw from the configured mound distance, and the `call` (`ball` or `strike`) made on that simulated crossing against the `zone` it was judged by. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.

## Games

A game module runs the game side of a session inside the tracker: it sees every session change and event, exchanges opaque bodies with the simulators that list it in their hello, and may add lines to the session summary. Modules are compiled in behind Cargo features, and the app has a page for each under `/games/<name>`.

`challenges` is the first, for pitching: the user pitches to a lineup of CPU batters, each with a goal such as throwing high or bringing the heat. The tracker judges every pitch against the goal, decides how the batter responds, and the simulator flies any batted ball and reports the plate appearance's result. Batters, goals, and lineups are in `challenges.toml` in the data directory, written with an example on first run.
