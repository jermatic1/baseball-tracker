"""HTTP training service for the tracker.

The tracker POSTs a gzipped tar with a prebuilt YOLO dataset (train/), an
optional held-out set (holdout/), and optionally the model it runs now
(current.onnx). One job runs at a time under runs/train-server/<id>/: fit with
train.py, export ONNX, score the new and current models on the held-out set.

  POST /train            -> 202 {"job": id} | 409 {"error": "busy"}
  GET  /jobs/<id>        -> {"state", "log", "result"}
  GET  /jobs/<id>/model  -> detector.onnx once done
  GET  /health           -> {"ok": true, "busy": bool}

The job itself runs in a child process (--job) so its output is the job log.
"""

import argparse
import json
import secrets
import shutil
import subprocess
import sys
import tarfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import train  # noqa: E402

PORT = 7882
RUNS = Path("runs/train-server")
LOG_LINES = 200
HIT_RADIUS_PX = 16
CONF = 0.25
CHUNK = 1 << 20


def centre(box):
    return (box[0] + box[2]) / 2, (box[1] + box[3]) / 2


def iou(a, b):
    w = min(a[2], b[2]) - max(a[0], b[0])
    h = min(a[3], b[3]) - max(a[1], b[1])
    if w <= 0 or h <= 0:
        return 0.0
    inter = w * h
    union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / union if union > 0 else 0.0


def average_precision(frames):
    """AP at IoU 0.5 over every detection: the area under the interpolated
    precision-recall curve sampled at 101 recall points."""
    n_truth = sum(len(f["truth"]) for f in frames)
    if n_truth == 0:
        return 0.0
    dets = sorted(
        ((d[4], k, d) for k, f in enumerate(frames) for d in f["dets"]), key=lambda t: -t[0]
    )
    used = [[False] * len(f["truth"]) for f in frames]
    tp = fp = 0
    curve = []
    for _, k, det in dets:
        best, best_iou = None, 0.5
        for j, truth in enumerate(frames[k]["truth"]):
            overlap = iou(det, truth)
            if not used[k][j] and overlap >= best_iou:
                best, best_iou = j, overlap
        if best is None:
            fp += 1
        else:
            used[k][best] = True
            tp += 1
        curve.append((tp / n_truth, tp / (tp + fp)))
    xs = [i / 100 for i in range(101)]
    ys = [max((p for r, p in curve if r >= x), default=0.0) for x in xs]
    return sum((ys[i] + ys[i + 1]) / 2 * (xs[i + 1] - xs[i]) for i in range(100))


def score_frames(frames):
    """frames: [{"truth": [xyxy], "dets": [xyxy+conf]}] in pixels."""
    ball = [f for f in frames if f["truth"]]
    empty = [f for f in frames if not f["truth"]]
    hits = 0
    for f in ball:
        centres = [centre(t) for t in f["truth"]]
        if any(
            (cx - tx) ** 2 + (cy - ty) ** 2 <= HIT_RADIUS_PX**2
            for d in f["dets"]
            if d[4] >= CONF
            for cx, cy in [centre(d)]
            for tx, ty in centres
        ):
            hits += 1
    false = sum(1 for f in empty for d in f["dets"] if d[4] >= CONF)
    return {
        "recall": hits / len(ball) if ball else 0.0,
        "false_per_empty": false / len(empty) if empty else 0.0,
        "map50": average_precision(frames),
    }


def label_boxes(path, width, height):
    boxes = []
    for line in path.read_text().splitlines() if path.exists() else []:
        parts = line.split()
        if len(parts) != 5:
            continue
        cx, cy, w, h = (float(v) for v in parts[1:])
        boxes.append(
            (
                (cx - w / 2) * width,
                (cy - h / 2) * height,
                (cx + w / 2) * width,
                (cy + h / 2) * height,
            )
        )
    return boxes


def score_model(model, holdout, device=None):
    """Run an exported ONNX over holdout/images at its own input shape."""
    import onnxruntime
    from ultralytics import YOLO

    session = onnxruntime.InferenceSession(str(model), providers=["CPUExecutionProvider"])
    shape = [int(v) for v in session.get_inputs()[0].shape[2:]]
    images = sorted((holdout / "images").glob("*.jpg"))
    extra = {} if device is None else {"device": device}
    yolo = YOLO(str(model), task="detect")
    frames = []
    for path in images:
        (result,) = yolo.predict(str(path), imgsz=shape, conf=0.001, verbose=False, **extra)
        height, width = result.orig_shape
        xyxy = result.boxes.xyxy.tolist()
        conf = result.boxes.conf.tolist()
        frames.append(
            {
                "truth": label_boxes(holdout / "labels" / f"{path.stem}.txt", width, height),
                "dets": [(*box, c) for box, c in zip(xyxy, conf)],
            }
        )
    score = score_frames(frames)
    balls = sum(1 for f in frames if f["truth"])
    print(f"{model.name}: {len(frames)} held-out frames ({balls} with a ball): {score}")
    return score


def run_job(job, epochs, imgsz, device):
    """Child process: unpack, fit, export, score; writes result.json."""
    bundle = job / "bundle"
    with tarfile.open(job / "bundle.tar.gz", "r:gz") as tar:
        tar.extractall(bundle, filter="data")
    manifest_path = bundle / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
    print(f"manifest: {manifest}")
    data = train.use_dataset(bundle / "train")
    frames = manifest.get("frames")
    if frames is None:
        frames = len(list((bundle / "train" / "images").glob("*/*.jpg")))
    best, ran = train.fit(data, epochs, imgsz, (job / "fit").resolve(), device)
    model = train.export_onnx(best, imgsz, job / "detector.onnx")
    holdout = bundle / "holdout"
    candidate = current = None
    if (holdout / "images").is_dir() and any((holdout / "images").glob("*.jpg")):
        candidate = score_model(model, holdout, device)
        if (bundle / "current.onnx").is_file():
            current = score_model(bundle / "current.onnx", holdout, device)
        else:
            print("no current.onnx; scoring the new model only")
    else:
        print("no holdout set; models not scored")
    result = {"candidate": candidate, "current": current, "epochs": ran, "frames": frames}
    (job / "result.json").write_text(json.dumps(result))


def log_tail(path, lines=LOG_LINES):
    if not path.exists():
        return ""
    text = path.read_bytes().decode(errors="replace")
    rows = [row.rsplit("\r", 1)[-1] for row in text.splitlines()]
    return "\n".join(rows[-lines:])


class Jobs:
    """One job at a time; state in memory, files under runs/."""

    def __init__(self, root, launch):
        self.root = root
        self.launch = launch
        self.lock = threading.Lock()
        self.jobs = {}

    def busy(self):
        with self.lock:
            return any(j["state"] in ("queued", "running") for j in self.jobs.values())

    def claim(self):
        with self.lock:
            if any(j["state"] in ("queued", "running") for j in self.jobs.values()):
                return None
            job_id = time.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(3)
            path = self.root / job_id
            path.mkdir(parents=True)
            self.jobs[job_id] = {"state": "queued", "dir": path}
            return job_id

    def set_state(self, job_id, state):
        with self.lock:
            self.jobs[job_id]["state"] = state

    def start(self, job_id):
        threading.Thread(target=self._run, args=(job_id,), daemon=True).start()

    def _run(self, job_id):
        job = self.jobs[job_id]["dir"]
        self.set_state(job_id, "running")
        try:
            ok = self.launch(job)
        except Exception as exc:
            with open(job / "log.txt", "a") as log:
                log.write(f"job failed to start: {exc}\n")
            ok = False
        self.set_state(job_id, "done" if ok and (job / "result.json").exists() else "failed")

    def get(self, job_id):
        with self.lock:
            job = self.jobs.get(job_id)
            return dict(job) if job else None


def child_launcher(epochs, imgsz, device):
    def launch(job):
        cmd = [sys.executable, str(Path(__file__).resolve()), "--job", str(job)]
        cmd += ["--epochs", str(epochs), "--imgsz", str(imgsz)]
        if device:
            cmd += ["--device", device]
        with open(job / "log.txt", "ab") as log:
            return subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT).returncode == 0

    return launch


class Handler(BaseHTTPRequestHandler):
    jobs = None

    def send_json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        parts = [p for p in self.path.split("?")[0].split("/") if p]
        if parts == ["health"]:
            return self.send_json(200, {"ok": True, "busy": self.jobs.busy()})
        if len(parts) in (2, 3) and parts[0] == "jobs":
            job = self.jobs.get(parts[1])
            if job is None:
                return self.send_json(404, {"error": "no such job"})
            if len(parts) == 3 and parts[2] == "model":
                return self.send_model(job)
            if len(parts) == 2:
                return self.send_status(job)
        self.send_json(404, {"error": "not found"})

    def send_status(self, job):
        result = None
        if job["state"] == "done":
            result = json.loads((job["dir"] / "result.json").read_text())
        self.send_json(
            200, {"state": job["state"], "log": log_tail(job["dir"] / "log.txt"), "result": result}
        )

    def send_model(self, job):
        model = job["dir"] / "detector.onnx"
        if job["state"] != "done" or not model.is_file():
            return self.send_json(404, {"error": "model not ready"})
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(model.stat().st_size))
        self.end_headers()
        with open(model, "rb") as f:
            shutil.copyfileobj(f, self.wfile)

    def do_POST(self):
        if self.path.split("?")[0].rstrip("/") != "/train":
            return self.send_json(404, {"error": "not found"})
        length = self.headers.get("Content-Length")
        if length is None:
            self.close_connection = True
            return self.send_json(411, {"error": "Content-Length required"})
        job_id = self.jobs.claim()
        if job_id is None:
            self.close_connection = True
            return self.send_json(409, {"error": "busy"})
        job = self.jobs.get(job_id)["dir"]
        remaining = int(length)
        with open(job / "bundle.tar.gz", "wb") as out:
            while remaining > 0:
                chunk = self.rfile.read(min(CHUNK, remaining))
                if not chunk:
                    break
                out.write(chunk)
                remaining -= len(chunk)
        if remaining > 0:
            self.jobs.set_state(job_id, "failed")
            self.close_connection = True
            return self.send_json(400, {"error": "body ended early"})
        self.jobs.start(job_id)
        self.send_json(202, {"job": job_id})


def serve(bind, jobs):
    host, _, port = bind.rpartition(":")
    handler = type("BoundHandler", (Handler,), {"jobs": jobs})
    return ThreadingHTTPServer((host or "0.0.0.0", int(port or PORT)), handler)


def self_test():
    frames = [
        {"truth": [(90, 90, 110, 110)], "dets": [(92, 91, 112, 111, 0.9)]},
        {"truth": [(300, 200, 320, 220)], "dets": [(400, 200, 420, 220, 0.8)]},
        {"truth": [], "dets": [(10, 10, 20, 20, 0.5), (30, 30, 40, 40, 0.1)]},
        {"truth": [], "dets": []},
    ]
    score = score_frames(frames)
    assert score["recall"] == 0.5, score
    assert score["false_per_empty"] == 0.5, score
    assert abs(score["map50"] - 0.505) < 1e-9, score
    near = [{"truth": [(0, 0, 32, 32)], "dets": [(10, 10, 42, 42, 0.3)]}]
    assert score_frames(near)["recall"] == 1.0
    assert score_frames(near)["map50"] == 0.0
    assert score_frames([{"truth": [], "dets": []}])["map50"] == 0.0

    import tempfile
    import urllib.error
    import urllib.request

    gate = threading.Event()

    def launch(job):
        (job / "detector.onnx").write_bytes(b"onnx")
        (job / "result.json").write_text(json.dumps({"epochs": 1}))
        gate.wait(5)
        return True

    def call(method, path, body=None):
        req = urllib.request.Request(base + path, data=body, method=method)
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as err:
            return err.code, err.read()

    with tempfile.TemporaryDirectory() as tmp:
        server = serve("127.0.0.1:0", Jobs(Path(tmp), launch))
        base = f"http://127.0.0.1:{server.server_address[1]}"
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            status, body = call("POST", "/train", b"x")
            assert status == 202, (status, body)
            job = json.loads(body)["job"]
            assert call("POST", "/train", b"x")[0] == 409
            assert json.loads(call("GET", "/health")[1])["busy"] is True
            assert call("GET", f"/jobs/{job}/model")[0] == 404
            gate.set()
            for _ in range(50):
                state = json.loads(call("GET", f"/jobs/{job}")[1])
                if state["state"] == "done":
                    break
                time.sleep(0.05)
            assert state["result"] == {"epochs": 1}, state
            assert call("GET", f"/jobs/{job}/model") == (200, b"onnx")
            assert call("GET", "/jobs/nope")[0] == 404
        finally:
            server.shutdown()
            server.server_close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bind", default=f"0.0.0.0:{PORT}", help="host:port to listen on")
    parser.add_argument("--epochs", type=int, default=40)
    parser.add_argument("--imgsz", type=int, default=train.FRAME_WIDTH)
    parser.add_argument("--device", help="ultralytics device, e.g. 0 or cpu; default picks")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--job", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("ok")
        return
    if args.job:
        run_job(args.job, args.epochs, args.imgsz, args.device)
        return
    jobs = Jobs(RUNS.resolve(), child_launcher(args.epochs, args.imgsz, args.device))
    server = serve(args.bind, jobs)
    print(f"training service on {args.bind}, jobs under {RUNS}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
