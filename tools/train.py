"""Fine-tune YOLO26n on labeled tracker frames and export ONNX.

Reads <data>/labels.json written by `tracker label` from one or more data
directories and trains on them together. Clips listed in `empty_clips` give
background frames with no ball. `--dataset <dir>` trains on a prebuilt YOLO
dataset (<dir>/data.yaml) instead.
Python is only this export step. The tracker loads the ONNX.
"""

import argparse
import json
import shutil
import sys
from pathlib import Path

VAL_EVERY = 5
FRAME_WIDTH = 640
FRAME_HEIGHT = 400
CLASSES = ["ball"]
EMPTY_CLIP_STRIDE = 10
EMPTY_CLIP_CAP = 2


def mark_class(mark):
    return mark.get("cls") or CLASSES[0]


def clicked_balls(marks):
    """Ball marks with a clicked centre. Unmarked frames are never used."""
    return [
        {**m, "cls": mark_class(m)}
        for m in marks
        if m.get("kind") == "ball" and m.get("cx") is not None and m.get("cy") is not None
    ]


def frame_rows(marks):
    """Every ball on each marked frame, keyed (clip, index) in the order the
    frame's first mark appears: balls, then frames marked empty."""
    rows = {}
    for m in clicked_balls(marks):
        rows.setdefault((m["clip"], m["index"]), []).append((m["cls"], m["cx"], m["cy"]))
    for m in marks:
        if m.get("kind") == "empty":
            rows.setdefault((m["clip"], m["index"]), [])
    return rows


def split_by_clip(items):
    """Every fifth clip to validation, whole, so no validation frame has a
    near-identical neighbour in training. One clip gives its first frame."""
    clips = list(dict.fromkeys((item["source"], item["clip"]) for item in items))
    val_clips = set(clips[::VAL_EVERY]) if len(clips) > 1 else set()
    val = [item for item in items if (item["source"], item["clip"]) in val_clips]
    train = [item for item in items if (item["source"], item["clip"]) not in val_clips]
    if not val:
        val = train[:1]
    return train, val


def self_test():
    marks = [
        {"clip": "0001", "index": 0, "kind": "ball", "cx": 0.0, "cy": 10.0},
        {"clip": "0001", "index": 4, "kind": "ball", "cx": 40.0, "cy": 10.0},
        {"clip": "0001", "index": 2, "kind": "empty"},
    ]
    assert [m["index"] for m in clicked_balls(marks)] == [0, 4]
    two = marks + [{"clip": "0001", "index": 4, "kind": "ball", "cx": 90.0, "cy": 5.0}]
    rows = frame_rows(two)
    assert rows[("0001", 4)] == [("ball", 40.0, 10.0), ("ball", 90.0, 5.0)], rows
    assert rows[("0001", 2)] == []
    items = [{"source": 0, "clip": c, "index": 0} for c in ["a", "b", "c", "d", "e", "f"]]
    train, val = split_by_clip(items)
    assert [i["clip"] for i in val] == ["a", "f"], val
    assert len(train) == 4
    one = [{"source": 0, "clip": "a", "index": k} for k in range(3)]
    assert split_by_clip(one) == (one, one[:1])
    assert even_sample(list(range(100)), 4) == [0, 25, 50, 75]
    assert even_sample([3, 7], 40) == [3, 7]
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        clip_dir = Path(tmp)
        assert empty_clip_frames(clip_dir, 25) == [0, 10]
        frames = [{"index": k, "balls": [{}] if k % 2 else []} for k in range(200)]
        (clip_dir / "detections.json").write_text(json.dumps({"frames": frames}))
        picked = empty_clip_frames(clip_dir, 200)
        assert len(picked) == EMPTY_CLIP_CAP and all(k % 2 for k in picked), picked
        (clip_dir / "data.yaml").write_text("path: /elsewhere/train\ntrain: images/train\n")
        data = use_dataset(clip_dir)
        lines = data.read_text().splitlines()
        assert lines == [f"path: {clip_dir.resolve()}", "train: images/train"], lines


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
        text = "".join(
            yolo_line(cls, cx, cy, item["box"], item["width"], item["height"])
            for cls, cx, cy in item["balls"]
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
    """Labelled frames of one data directory."""
    box = int(labels.get("box_px") or labels.get("box") or 32)
    marks = labels.get("marks", [])
    rows = frame_rows(marks)
    marked = {(m["clip"], m["index"]) for m in marks}
    for clip in labels.get("empty_clips", []):
        meta = json.loads((session / "clips" / clip / "meta.json").read_text())
        for index in empty_clip_frames(session / "clips" / clip, meta["frame_count"]):
            if (clip, index) not in marked:
                rows[(clip, index)] = []
    items = []
    metas = {}
    for (clip, index), balls in rows.items():
        if clip not in metas:
            metas[clip] = json.loads((session / "clips" / clip / "meta.json").read_text())
        meta = metas[clip]
        items.append(
            {
                "source": source,
                "clip": clip,
                "index": index,
                "kind": "ball" if balls else "empty",
                "balls": balls,
                "box": box,
                "width": meta["width"],
                "height": meta["height"],
                "image": load_frame(session / "clips" / clip, index, meta["width"], meta["height"]),
            }
        )
    return items


def build_dataset(sessions):
    """Merge every session's labels into <first session>/train/."""
    items = []
    for source, session in enumerate(sessions):
        labels_path = session / "labels.json"
        if not labels_path.exists():
            raise SystemExit(f"no labels at {labels_path}; run task label first")
        got = session_items(source, session, json.loads(labels_path.read_text()))
        empties = sum(1 for item in got if item["kind"] == "empty")
        print(f"{session}: {len(got) - empties} ball frames, {empties} empty frames")
        items.extend(got)
    clicks = sum(len(item["balls"]) for item in items)
    if clicks < 8:
        raise SystemExit("need at least 8 ball clicks before training")
    items.sort(key=lambda item: (item["source"], item["clip"], item["index"]))
    train, val = split_by_clip(items)
    root = sessions[0] / "train"
    for generated in ("images", "labels"):
        shutil.rmtree(root / generated, ignore_errors=True)
    write_split(root, "train", train)
    write_split(root, "val", val)
    data = root / "data.yaml"
    names = "".join(f"  {k}: {name}\n" for k, name in enumerate(CLASSES))
    data.write_text(f"path: {root}\ntrain: images/train\nval: images/val\nnames:\n{names}")
    return data, len(train), len(val)


def use_dataset(root):
    """Point a prebuilt dataset's data.yaml at where it now lives."""
    data = root / "data.yaml"
    if not data.is_file():
        raise SystemExit(f"no data.yaml in {root}")
    lines = [line for line in data.read_text().splitlines() if not line.startswith("path:")]
    data.write_text("\n".join([f"path: {root.resolve()}", *lines]) + "\n")
    return data


def fit(data, epochs, imgsz, project, device=None):
    """Fine-tune YOLO26n on data.yaml under <project>/run; returns best.pt and
    the number of epochs run."""
    try:
        from ultralytics import YOLO
    except ImportError as exc:
        raise SystemExit(f"cannot import ultralytics from {sys.executable}: {exc}") from exc
    model = YOLO("yolo26n.pt")
    extra = {} if device is None else {"device": device}
    model.train(
        data=str(data),
        epochs=epochs,
        imgsz=imgsz,
        batch=8,
        project=str(project),
        name="run",
        exist_ok=True,
        **extra,
    )
    return best_weights(project, model.trainer), model.trainer.epoch + 1


def best_weights(project, trainer=None):
    candidates = []
    if trainer is not None:
        candidates.append(Path(getattr(trainer, "best", "")))
        save_dir = getattr(trainer, "save_dir", None)
        if save_dir:
            candidates.append(Path(save_dir) / "weights" / "best.pt")
    candidates.extend(
        [
            Path("runs/detect") / project / "run" / "weights" / "best.pt",
            Path(project) / "run" / "weights" / "best.pt",
        ]
    )
    for path in candidates:
        if path.is_file():
            return path
    raise SystemExit("best.pt not found; training did not write weights")


def export_onnx(best, imgsz, dest=Path("models/detector.onnx")):
    """Export at the frame's own shape: height rounded up to the stride, so a
    640x400 frame is padded by a few rows and never scaled."""
    from ultralytics import YOLO

    height = -(-FRAME_HEIGHT // 32) * 32 if imgsz == FRAME_WIDTH else imgsz
    exported = YOLO(str(best)).export(format="onnx", imgsz=[height, imgsz])
    dest.parent.mkdir(parents=True, exist_ok=True)
    Path(exported).replace(dest)
    print(dest)
    return dest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "sessions",
        nargs="*",
        type=Path,
        help="data directories with labels.json; the dataset goes under the first one's train/",
    )
    parser.add_argument(
        "--dataset", type=Path, help="prebuilt YOLO dataset with data.yaml; skips the label build"
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
    if args.dataset:
        project = args.dataset
    elif args.sessions:
        project = args.sessions[0] / "train"
    else:
        raise SystemExit("session path or --dataset required")
    if args.export_only:
        export_onnx(best_weights(project), args.imgsz)
        return
    if args.dataset:
        data = use_dataset(args.dataset)
    else:
        data, n_train, n_val = build_dataset(args.sessions)
        print(f"dataset train={n_train} val={n_val}")
    best, _ = fit(data, args.epochs, args.imgsz, project)
    export_onnx(best, args.imgsz)


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        sys.exit(0)
