"""Fine-tune YOLO26n on labeled tracker frames and export ONNX.

Reads sessions/<name>/labels.json written by `tracker label`.
Python is only this export step. The tracker loads the ONNX.
"""

import argparse
import json
import sys
from pathlib import Path

MAX_GAP = 12


def interpolate(marks):
    by_clip = {}
    for mark in marks:
        by_clip.setdefault(mark["clip"], []).append(mark)
    out = []
    for clip, rows in by_clip.items():
        rows = sorted(rows, key=lambda m: m["index"])
        balls = [m for m in rows if m.get("kind") == "ball" and m.get("cx") is not None]
        for mark in balls:
            out.append({**mark, "filled": False})
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


def yolo_line(cx, cy, box, width, height):
    return f"0 {cx / width:.6f} {cy / height:.6f} {box / width:.6f} {box / height:.6f}\n"


def write_split(root, name, items):
    img_dir = root / "images" / name
    lbl_dir = root / "labels" / name
    img_dir.mkdir(parents=True, exist_ok=True)
    lbl_dir.mkdir(parents=True, exist_ok=True)
    for item in items:
        stem = f"{item['clip']}_{item['index']:05d}"
        item["image"].save(img_dir / f"{stem}.jpg")
        text = ""
        if item["kind"] == "ball":
            text = yolo_line(item["cx"], item["cy"], item["box"], item["width"], item["height"])
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


def build_dataset(session, labels):
    box = int(labels.get("box_px") or labels.get("box") or 32)
    marks = labels["marks"]
    balls = interpolate(marks)
    empties = [m for m in marks if m.get("kind") == "empty"]
    if len([m for m in balls if not m["filled"]]) < 8:
        raise SystemExit("need at least 8 ball clicks before training")
    items = []
    metas = {}
    for mark in balls + empties:
        clip = mark["clip"]
        if clip not in metas:
            meta = json.loads((session / "clips" / clip / "meta.json").read_text())
            metas[clip] = meta
        meta = metas[clip]
        items.append(
            {
                "clip": clip,
                "index": mark["index"],
                "kind": mark["kind"],
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
    items.sort(key=lambda item: (item["clip"], item["index"]))
    val = [item for i, item in enumerate(items) if i % 5 == 0]
    train = [item for i, item in enumerate(items) if i % 5 != 0]
    if not val:
        val = train[:1]
    root = session / "train"
    write_split(root, "train", train)
    write_split(root, "val", val)
    data = root / "data.yaml"
    data.write_text(
        f"path: {root}\ntrain: images/train\nval: images/val\nnames:\n  0: ball\n"
    )
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
    from ultralytics import YOLO

    exported = YOLO(str(best)).export(format="onnx", imgsz=imgsz)
    out = Path("models")
    out.mkdir(exist_ok=True)
    dest = out / "ball.onnx"
    Path(exported).replace(dest)
    print(dest)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("session", nargs="?", type=Path)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--export-only", action="store_true")
    parser.add_argument("--epochs", type=int, default=40)
    parser.add_argument("--imgsz", type=int, default=1280)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("ok")
        return
    if args.session is None:
        raise SystemExit("session path required")
    if args.export_only:
        export_onnx(best_weights(args.session), args.imgsz)
        return
    try:
        from ultralytics import YOLO
    except ImportError as exc:
        raise SystemExit(f"cannot import ultralytics from {sys.executable}: {exc}") from exc
    labels_path = args.session / "labels.json"
    if not labels_path.exists():
        raise SystemExit(f"no labels at {labels_path}; run task label first")
    labels = json.loads(labels_path.read_text())
    data, n_train, n_val = build_dataset(args.session, labels)
    print(f"dataset train={n_train} val={n_val}")
    model = YOLO("yolo26n.pt")
    model.train(
        data=str(data),
        epochs=args.epochs,
        imgsz=args.imgsz,
        batch=8,
        project=str(args.session / "train"),
        name="run",
        exist_ok=True,
    )
    export_onnx(best_weights(args.session, model.trainer), args.imgsz)


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        sys.exit(0)
