#!/usr/bin/env python3
"""Offline model preparation only; never imported or executed by the gateway."""
import argparse
import hashlib
import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODEL_ID = "shibing624/bert4ner-base-chinese"


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def check_source(source):
    snapshot = json.loads((Path(__file__).with_name("validated-model.json")).read_text())
    for name, expected in snapshot["source_sha256"].items():
        if sha256(source / name) != expected:
            raise RuntimeError(f"Source model differs from the validated snapshot: {name}")
    print(json.dumps(dict(model=MODEL_ID, source_files_match=True)))


def export(source, output):
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch
    from transformers import AutoModelForTokenClassification, AutoTokenizer

    torch.set_num_threads(1)
    options = dict(local_files_only=True, trust_remote_code=False)
    tokenizer = AutoTokenizer.from_pretrained(source, use_fast=True, **options)
    model = AutoModelForTokenClassification.from_pretrained(
        source, attn_implementation="eager", **options
    ).eval()
    labels = [model.config.id2label[i] for i in range(model.config.num_labels)]
    assert model.config.model_type == "bert"
    assert set(labels) == {"O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC", "B-TIME", "I-TIME"}
    output.mkdir(parents=True, exist_ok=True)
    tokenizer.model_max_length = 512
    tokenizer.save_pretrained(output)
    model.config.to_json_file(output / "config.json")

    class Logits(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.bert = model

        def forward(self, input_ids, attention_mask, token_type_ids):
            return self.bert(input_ids=input_ids, attention_mask=attention_mask,
                             token_type_ids=token_type_ids).logits

    names = ["input_ids", "attention_mask", "token_type_ids"]
    inputs = tokenizer("张三在清华大学工作。", return_tensors="pt")
    with torch.no_grad():
        torch.onnx.export(
            Logits().eval(), tuple(inputs[name] for name in names),
            str(output / "model.onnx"), input_names=names, output_names=["logits"],
            dynamic_axes={name: {0: "batch", 1: "sequence"} for name in [*names, "logits"]},
            opset_version=17, dynamo=False,
        )
    onnx.checker.check_model(str(output / "model.onnx"))
    session_options = ort.SessionOptions()
    session_options.intra_op_num_threads = 1
    session = ort.InferenceSession(str(output / "model.onnx"), session_options,
                                   providers=["CPUExecutionProvider"])
    maximum_logit_error = 0.0
    maximum_probability_error = 0.0
    windows = 0
    for request in samples():
        chunks = tokenizer(request["text"], truncation=True, max_length=512, stride=64,
                           return_overflowing_tokens=True)
        for index in range(len(chunks["input_ids"])):
            feed = {name: np.asarray([chunks[name][index]], dtype=np.int64) for name in names}
            with torch.no_grad():
                expected = model(**{name: torch.from_numpy(value) for name, value in feed.items()}).logits.numpy()
            actual = session.run(["logits"], feed)[0]
            maximum_logit_error = max(maximum_logit_error, float(np.max(np.abs(expected - actual))))
            def softmax(value):
                exp = np.exp(value - value.max(axis=-1, keepdims=True))
                return exp / exp.sum(axis=-1, keepdims=True)
            maximum_probability_error = max(maximum_probability_error,
                                             float(np.max(np.abs(softmax(expected) - softmax(actual)))))
            if not np.array_equal(expected.argmax(-1), actual.argmax(-1)):
                raise RuntimeError("PyTorch / ONNX token label mismatch")
            windows += 1
    if maximum_logit_error > 1e-4 or maximum_probability_error > 2e-5:
        raise RuntimeError("PyTorch / ONNX numerical tolerance exceeded")
    manifest = dict(model_id=MODEL_ID, precision="float32", max_length=512, stride=64,
                    labels=labels, sha256={name: sha256(output / name)
                    for name in ["model.onnx", "tokenizer.json", "config.json"]})
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    report = dict(windows=windows, token_labels_identical=True,
                  max_logit_error=maximum_logit_error,
                  max_probability_error=maximum_probability_error,
                  torch=torch.__version__, onnx=onnx.__version__, onnxruntime=ort.__version__)
    (output / "export-validation.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


def samples():
    """Read the committed inputs shared with Rust regressions."""
    path = ROOT / "tests/fixtures/native-detector-reference.jsonl"
    for line in path.read_text(encoding="utf-8").splitlines():
        yield json.loads(line)["request"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check-source", action="store_true",
                        help="Verify source hashes against the validated snapshot; no Python ML dependencies")
    args = parser.parse_args()
    if args.check_source:
        if args.output:
            parser.error("--output and --check-source are separate operations")
        check_source(args.source)
    elif args.output:
        os.environ.setdefault("OMP_NUM_THREADS", "1")
        export(args.source, args.output)
    else:
        parser.error("--output or --check-source is required")
