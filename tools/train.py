"""Fine-tune YOLO26n on labeled tracker frames and export ONNX.

Reads <data>/labels.json written by `tracker label` from one or more data
directories and trains on them together. Clips listed in `empty_clips` give
background frames with no ball.
Python is only this export step. The tracker loads the ONNX.
"""

import argparse
import json
import shutil
import sys
from pathlib import Path

MAX_GAP = 12
FRAME_WIDTH = 640
FRAME_HEIGHT = 400
CLASSES = ["ball"]
EMPTY_CLIP_STRIDE = 10
EMPTY_CLIP_CAP = 2


def mark_class(mark):
    return mark.get("cls") or CLASSES[0]


def interpolate(marks):
    """Clicked boxes plus straight-line fills between clicks of one class."""
    by_clip = {}
    for mark in marks:
        by_clip.setdefault(mark["clip"], []).append(mark)
    out = []
    for clip, rows in by_clip.items():
        rows = sorted(rows, key=lambda m: m["index"])
        clicks = [m for m in rows if m.get("kind") == "ball" and m.get("cx") is not None]
        for mark in clicks:
            out.append({**mark, "cls": mark_class(mark), "filled": False})
        for cls in sorted({mark_class(m) for m in clicks}):
            balls = [m for m in clicks if mark_class(m) == cls]
            out.extend(fill_gaps(clip, cls, rows, balls))
    return out


def fill_gaps(clip, cls, rows, balls):
    out = []
    for a, b in zip(balls, balls[1:]):
        gap = b["index"] - a["index"]
        if gap < 2 or gap > MAX_GAP:
            continue
        if any(
            m.get("kind") == "empty" and a["index"] < m["index"] < b["index"] for m in rows
        ):
            continue
        for index in range(a["index"] + 1, b["index"]):
            t = (index - a["index"]) / gap
            out.append(
                {
                    "clip": clip,
                    "index": index,
                    "kind": "ball",
                    "cls": cls,
                    "cx": a["cx"] + (b["cx"] - a["cx"]) * t,
                    "cy": a["cy"] + (b["cy"] - a["cy"]) * t,
                    "filled": True,
                }
            )
    return out


def self_test():
    marks = [
        {"clip": "0001", "index": 0, "kind": "ball", "cx": 0.0, "cy": 10.0},
        {"clip": "0001", "index": 4, "kind": "ball", "cx": 40.0, "cy": 10.0},
    ]
    filled = [m["index"] for m in interpolate(marks) if m["filled"]]
    assert filled == [1, 2, 3], filled
    mid = next(m for m in interpolate(marks) if m["index"] == 2)
    assert abs(mid["cx"] - 20.0) < 1e-6
    broken = marks + [{"clip": "0001", "index": 2, "kind": "empty"}]
    assert all(not m["filled"] for m in interpolate(broken))
    assert even_sample(list(range(100)), 4) == [0, 25, 50, 75]
    assert even_sample([3, 7], 40) == [3, 7]
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        clip_dir = Path(tmp)
        assert empty_clip_frames(clip_dir, 25) == [0, 10, 20]
        frames = [{"index": k, "balls": [{}] if k % 2 else []} for k in range(200)]
        (clip_dir / "detections.json").write_text(json.dumps({"frames": frames}))
        picked = empty_clip_frames(clip_dir, 200)
        assert len(picked) == EMPTY_CLIP_CAP and all(k % 2 for k in picked), picked


def even_sample(items, cap):
    if len(items) <= cap:
        return items
    return [items[k * len(items) // cap] for k in range(cap)]


def empty_clip_frames(clip_dir, frame_count):
    """Frames of a clip with no ball: those the live detector fired on, else
    every tenth, at most EMPTY_CLIP_CAP spread over the clip."""
    path = clip_dir / "detections.json"
    frames = []
    if path.exists():
        rows = json.loads(path.read_text()).get("frames", [])
        frames = sorted(
            r["index"] for r in rows if r.get("balls") and 0 <= r["index"] < frame_count
        )
    else:
        frames = list(range(0, frame_count, EMPTY_CLIP_STRIDE))
    return even_sample(frames, EMPTY_CLIP_CAP)


def yolo_line(cls, cx, cy, box, width, height):
    if cls not in CLASSES:
        raise SystemExit(f"unknown class {cls!r}; known: {', '.join(CLASSES)}")
    k = CLASSES.index(cls)
    return f"{k} {cx / width:.6f} {cy / height:.6f} {box / width:.6f} {box / height:.6f}\n"


def write_split(root, name, items):
    img_dir = root / "images" / name
    lbl_dir = root / "labels" / name
    img_dir.mkdir(parents=True, exist_ok=True)
    lbl_dir.mkdir(parents=True, exist_ok=True)
    for item in items:
        stem = f"s{item['source']}_{item['clip']}_{item['index']:05d}"
        item["image"].save(img_dir / f"{stem}.jpg")
        text = ""
        if item["kind"] == "ball":
            text = yolo_line(
                item["cls"], item["cx"], item["cy"], item["box"], item["width"], item["height"]
            )
        (lbl_dir / f"{stem}.txt").write_text(text)


def load_frame(clip_dir, index, width, height):
    from PIL import Image

    n = width * height
    with open(clip_dir / "left.gray", "rb") as f:
        f.seek(index * n)
        raw = f.read(n)
    if len(raw) != n:
        raise RuntimeError(f"short frame {clip_dir} {index}")
    return Image.frombytes("L", (width, height), raw).convert("RGB")


def session_items(source, session, labels):
    """Labelled frames of one data directory, and its count of ball clicks."""
    box = int(labels.get("box_px") or labels.get("box") or 32)
    marks = labels.get("marks", [])
    balls = interpolate(marks)
    rows = balls + [m for m in marks if m.get("kind") == "empty"]
    marked = {(m["clip"], m["index"]) for m in marks}
    for clip in labels.get("empty_clips", []):
        meta = json.loads((session / "clips" / clip / "meta.json").read_text())
        for index in empty_clip_frames(session / "clips" / clip, meta["frame_count"]):
            if (clip, index) not in marked:
                rows.append({"clip": clip, "index": index, "kind": "empty"})
    items = []
    metas = {}
    for mark in rows:
        clip = mark["clip"]
        if clip not in metas:
            metas[clip] = json.loads((session / "clips" / clip / "meta.json").read_text())
        meta = metas[clip]
        items.append(
            {
                "source": source,
                "clip": clip,
                "index": mark["index"],
                "kind": mark["kind"],
                "cls": mark_class(mark),
                "cx": mark.get("cx"),
                "cy": mark.get("cy"),
                "box": box,
                "width": meta["width"],
                "height": meta["height"],
                "image": load_frame(
                    session / "clips" / clip, mark["index"], meta["width"], meta["height"]
                ),
            }
        )
    return items, len([m for m in balls if not m["filled"]])


def build_dataset(sessions):
    """Merge every session's labels into <first session>/train/."""
    items = []
    clicks = 0
    for source, session in enumerate(sessions):
        labels_path = session / "labels.json"
        if not labels_path.exists():
            raise SystemExit(f"no labels at {labels_path}; run task label first")
        got, n = session_items(source, session, json.loads(labels_path.read_text()))
        empties = sum(1 for item in got if item["kind"] == "empty")
        print(f"{session}: {len(got) - empties} ball frames, {empties} empty frames")
        items.extend(got)
        clicks += n
    if clicks < 8:
        raise SystemExit("need at least 8 ball clicks before training")
    items.sort(key=lambda item: (item["source"], item["clip"], item["index"]))
    val = [item for i, item in enumerate(items) if i % 5 == 0]
    train = [item for i, item in enumerate(items) if i % 5 != 0]
    if not val:
        val = train[:1]
    root = sessions[0] / "train"
    for generated in ("images", "labels"):
        shutil.rmtree(root / generated, ignore_errors=True)
    write_split(root, "train", train)
    write_split(root, "val", val)
    data = root / "data.yaml"
    names = "".join(f"  {k}: {name}\n" for k, name in enumerate(CLASSES))
    data.write_text(f"path: {root}\ntrain: images/train\nval: images/val\nnames:\n{names}")
    return data, len(train), len(val)


def best_weights(session, trainer=None):
    candidates = []
    if trainer is not None:
        candidates.append(Path(getattr(trainer, "best", "")))
        save_dir = getattr(trainer, "save_dir", None)
        if save_dir:
            candidates.append(Path(save_dir) / "weights" / "best.pt")
    candidates.extend(
        [
            Path("runs/detect") / session / "train" / "run" / "weights" / "best.pt",
            session / "train" / "run" / "weights" / "best.pt",
        ]
    )
    for path in candidates:
        if path.is_file():
            return path
    raise SystemExit("best.pt not found; training did not write weights")


def export_onnx(best, imgsz):
    """Export at the frame's own shape: height rounded up to the stride, so a
    640x400 frame is padded by a few rows and never scaled."""
    from ultralytics import YOLO

    height = -(-FRAME_HEIGHT // 32) * 32 if imgsz == FRAME_WIDTH else imgsz
    exported = YOLO(str(best)).export(format="onnx", imgsz=[height, imgsz])
    out = Path("models")
    out.mkdir(exist_ok=True)
    dest = out / "ball.onnx"
    Path(exported).replace(dest)
    print(dest)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "sessions",
        nargs="*",
        type=Path,
        help="data directories with labels.json; the dataset goes under the first one's train/",
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--export-only", action="store_true")
    parser.add_argument("--epochs", type=int, default=40)
    parser.add_argument("--imgsz", type=int, default=FRAME_WIDTH)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("ok")
        return
    if not args.sessions:
        raise SystemExit("session path required")
    session = args.sessions[0]
    if args.export_only:
        export_onnx(best_weights(session), args.imgsz)
        return
    try:
        from ultralytics import YOLO
    except ImportError as exc:
        raise SystemExit(f"cannot import ultralytics from {sys.executable}: {exc}") from exc
    data, n_train, n_val = build_dataset(args.sessions)
    print(f"dataset train={n_train} val={n_val}")
    model = YOLO("yolo26n.pt")
    model.train(
        data=str(data),
        epochs=args.epochs,
        imgsz=args.imgsz,
        batch=8,
        project=str(session / "train"),
        name="run",
        exist_ok=True,
    )
    export_onnx(best_weights(session, model.trainer), args.imgsz)


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        sys.exit(0)
