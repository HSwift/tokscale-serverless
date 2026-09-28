#!/usr/bin/env python3
"""Reference-only offline Qoder price calibration using user-provided JSONL.

No model list, measured prices, account settings, or network calls are bundled.
Only per-request assistant usage is used; cumulative result totals are ignored.
"""

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def read_samples(paths, credits_field):
    samples = {}
    files = set()
    for path in paths:
        if not path.exists():
            raise ValueError(f"Input does not exist: {path}")
        files.update(path.rglob("*.jsonl") if path.is_dir() else [path])
    for path in sorted(files):
        with path.open(encoding="utf-8") as source:
            for line_number, line in enumerate(source, 1):
                if not line.strip():
                    continue
                try:
                    event = json.loads(line)
                except ValueError as error:
                    raise ValueError(f"Invalid JSON at {path}:{line_number}") from error
                if not isinstance(event, dict) or event.get("type") != "assistant":
                    continue
                message = event.get("message") or {}
                if not isinstance(message, dict):
                    continue
                usage = message.get("usage") or {}
                if not isinstance(usage, dict) or usage.get("billable") is False:
                    continue
                model = message.get("model") or usage.get("model")
                if not isinstance(model, str) or not model.strip():
                    continue
                if any(str(name).casefold() == "auto" for name in (model, usage.get("model"))):
                    continue
                request_id = usage.get("request_id") or message.get("id") or event.get("uuid")
                if not isinstance(request_id, str) or not request_id:
                    continue
                values = [usage.get(key) for key in (
                    "input_tokens", "output_tokens", "cache_read_input_tokens", credits_field
                )]
                if not all(number(value) for value in values):
                    continue
                inp, out, cached, credits = values
                if inp + out <= 0 or credits <= 0 or cached > inp:
                    continue
                if any(value != int(value) for value in (inp, out, cached)):
                    continue
                # This three-rate model does not calibrate cache creation.
                if usage.get("cache_creation_input_tokens", 0) != 0:
                    continue
                samples[request_id] = {
                    "model": model,
                    "x": [inp - cached, out, cached],
                    "credits": credits,
                    "input": inp,
                    "ratio": usage.get("context_usage_ratio"),
                }
    return samples


def fit(samples):
    """Column-scaled least squares for three rates; reject deficient samples."""
    scales = [max(row["x"][i] for row in samples) or 1 for i in range(3)]
    x = [[row["x"][i] / scales[i] for i in range(3)] for row in samples]
    y = [row["credits"] for row in samples]
    matrix = [
        [sum(row[i] * row[j] for row in x) for j in range(3)]
        + [sum(row[i] * value for row, value in zip(x, y))]
        for i in range(3)
    ]
    for i in range(3):
        pivot = max(range(i, 3), key=lambda j: abs(matrix[j][i]))
        if abs(matrix[pivot][i]) < 1e-10:
            return None
        matrix[i], matrix[pivot] = matrix[pivot], matrix[i]
        for j in range(3):
            if i == j:
                continue
            factor = matrix[j][i] / matrix[i][i]
            for k in range(i, 4):
                matrix[j][k] -= factor * matrix[i][k]
    prices = [matrix[i][3] / matrix[i][i] / scales[i] for i in range(3)]
    if not all(math.isfinite(value) for value in prices):
        return None
    if prices[0] <= 0 or prices[1] <= 0 or prices[2] < 0:
        return None
    return prices


def max_error(prices, samples):
    return max(abs(sum(p * x for p, x in zip(prices, row["x"])) / row["credits"] - 1)
               for row in samples)


def calibrate(training, validation, credits_field):
    if training.keys() & validation.keys():
        raise ValueError("Training and validation share request IDs; use separate requests.")
    groups = [defaultdict(list), defaultdict(list)]
    for samples, group in zip((training, validation), groups):
        for row in samples.values():
            group[row["model"]].append(row)
    models = {}
    for model, rows in sorted(groups[0].items()):
        held_out = groups[1][model]
        if len(rows) < 5 or not held_out:
            continue
        prices = fit(rows)
        if prices is None or max_error(prices, rows + held_out) > 0.01:
            continue
        windows = set()
        for row in rows:
            ratio = row["ratio"]
            if number(ratio) and 0 < ratio <= 1 and row["input"] > 0:
                window = row["input"] / ratio
                if math.isfinite(window) and 1 <= window < 2**52:
                    if abs(window - round(window)) < 1e-6:
                        windows.add(round(window))
        models[model] = {
            "prices": dict(zip(("freshInput", "output", "cacheRead"), prices),
                           creditsField=credits_field),
            "windows": {"observed": sorted(windows)},
        }
    return {"schema": "qoder-token-estimates/2", "models": models}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--train", required=True, nargs="+", type=Path,
                        help="Training JSONL files/directories; at least five requests per model")
    parser.add_argument("--validate", required=True, nargs="+", type=Path,
                        help="Separate validation JSONL files/directories")
    parser.add_argument("--credits-field", required=True,
                        choices=("credits", "original_credits"))
    parser.add_argument("--output", required=True, type=Path,
                        help="New local coefficient file; existing files are never overwritten")
    args = parser.parse_args()
    try:
        training = read_samples(args.train, args.credits_field)
        validation = read_samples(args.validate, args.credits_field)
        result = calibrate(training, validation, args.credits_field)
        if not result["models"]:
            raise ValueError("No model passed validation. Supply varied, nonzero usage and separate "
                             "validation requests with the chosen credits field (see scripts/README.md).")
        with args.output.open("x", encoding="utf-8") as output:
            output.write(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n")
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")
    model_count = len({row["model"] for row in training.values()})
    print(f"Wrote {len(result['models'])}/{model_count} calibrated models; "
          "unvalidated models omitted. Review locally before installing.")


if __name__ == "__main__":
    main()
