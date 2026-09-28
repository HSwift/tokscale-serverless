"""Entirely synthetic fixtures: no measured rates or real model identifiers."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from qoder_calibrate import calibrate, read_samples


def event(request_id, fresh, output, cached, model="example-model"):
    return {
        "type": "assistant",
        "message": {
            "id": request_id,
            "model": model,
            "usage": {
                "request_id": request_id,
                "input_tokens": fresh + cached,
                "output_tokens": output,
                "cache_read_input_tokens": cached,
                "original_credits": fresh * 0.01 + output * 0.04 + cached * 0.002,
                "credits": fresh * 0.005 + output * 0.02 + cached * 0.001,
                "context_usage_ratio": (fresh + cached) / 100000,
            },
        },
    }


class CalibrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.train = self.root / "train.jsonl"
        self.validate = self.root / "validate.jsonl"
        self.write(self.train, [event(str(i), *values) for i, values in enumerate([
            (1000, 10, 0), (200, 500, 800), (100, 10, 2000),
            (500, 200, 300), (2000, 50, 100),
        ])])
        self.write(self.validate, [event("held-out", 400, 80, 1200)])

    def write(self, path, rows):
        path.write_text("\n".join(json.dumps(row) for row in rows), encoding="utf-8")

    def samples(self, field="original_credits"):
        return read_samples([self.train], field), read_samples([self.validate], field)

    def test_recovers_separate_prices_and_window_without_double_counting_cache(self):
        for field, expected in [("original_credits", [0.01, 0.04, 0.002]),
                                ("credits", [0.005, 0.02, 0.001])]:
            result = calibrate(*self.samples(field), field)
            model = result["models"]["example-model"]
            for key, price in zip(("freshInput", "output", "cacheRead"), expected):
                self.assertAlmostEqual(model["prices"][key], price)
            self.assertEqual(model["prices"]["creditsField"], field)
            self.assertEqual(model["windows"]["observed"], [100000])

    def test_deduplicates_and_excludes_auto_missing_fields_and_cumulative_totals(self):
        valid = event("same", 1000, 20, 800)
        missing = event("missing", 100, 10, 20)
        del missing["message"]["usage"]["original_credits"]
        invalid = event("invalid", 100, 10, 20)
        invalid["message"]["usage"]["cache_read_input_tokens"] = 9999
        cache_creation = event("cache-creation", 100, 10, 20)
        cache_creation["message"]["usage"]["cache_creation_input_tokens"] = 100
        zero = event("zero", 0, 0, 0)
        self.write(self.train, [valid, valid, event("auto", 100, 10, 20, "Auto"),
                                missing, invalid, cache_creation, zero,
                                {"type": "result", "usage": valid["message"]["usage"]}])
        self.assertEqual(list(read_samples([self.train], "original_credits")), ["same"])

    def test_rejects_overlapping_validation_and_nonidentifiable_prices(self):
        training, _ = self.samples()
        with self.assertRaisesRegex(ValueError, "share request IDs"):
            calibrate(training, training, "original_credits")
        self.write(self.train, [event(str(i), 100, 10, 20) for i in range(5)])
        self.assertEqual(calibrate(*self.samples(), "original_credits")["models"], {})

    def test_bad_holdout_is_not_used_to_refit(self):
        changed = event("held-out", 400, 80, 1200)
        changed["message"]["usage"]["original_credits"] *= 2
        self.write(self.validate, [changed])
        self.assertEqual(calibrate(*self.samples(), "original_credits")["models"], {})

    def test_cli_output_contains_only_coefficients_and_does_not_overwrite(self):
        output = self.root / "qoder-coeffs.json"
        command = [sys.executable, str(Path(__file__).with_name("qoder_calibrate.py")),
                   "--train", str(self.train), "--validate", str(self.validate),
                   "--credits-field", "original_credits", "--output", str(output)]
        run = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        contents = output.read_text(encoding="utf-8")
        self.assertEqual(set(json.loads(contents)), {"schema", "models"})
        self.assertNotIn(str(self.root), contents)
        self.assertNotIn("request_id", contents)
        again = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(again.returncode, 0)
        self.assertEqual(output.read_text(encoding="utf-8"), contents)


if __name__ == "__main__":
    unittest.main()
