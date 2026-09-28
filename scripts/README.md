# Qoder calibration reference

[简体中文](README_CN.md)

`qoder_calibrate.py` is an optional, offline Python 3 reference for fitting your
own per-model prices. It includes no model inventory, measured coefficients,
account configuration, or sample transcripts. It makes no network requests and
is not included in the collector's release archives.

1. Enable `QODER_EXPOSE_TOKEN_USAGE=1` for your Qoder CLI process and explicitly
   select a fixed model available to your account. Generate your own requests
   with short/long input, short/long output, and repeated context to exercise
   cache reads. These model calls consume your credits; the script does not
   launch them. Omit Auto and other routing aliases.
2. Copy the relevant session JSONL files from your Qoder projects directory into
   a local `train` directory. The usual source is `~/.qoder/projects/**/*.jsonl`,
   including `subagents`; use your configured location if different. Collect
   at least five completed requests per model with real input/output/cache-read
   counts and the selected credits field. Generate at least one separate request
   per model in a new session and copy those files into a `validate` directory.
   Keep these directories outside this repository. Do not split copies of the
   same request between the two sets.
3. Run from the repository root, replacing the paths with your local directories:

   ```sh
   python3 scripts/qoder_calibrate.py \
     --train /path/to/train \
     --validate /path/to/validate \
     --credits-field original_credits \
     --output /path/to/qoder-coeffs.json
   ```

   On Windows, use `py -3` and your own paths. Choose `original_credits` for
   prices before discounts, or `credits` for billed prices. The selected field
   must exist in both calibration and later records; there is no substitution.
4. Review the generated file locally, then copy it beside the collector's
   `device.json`, or point `TOKSCALE_QODER_COEFFS` to it. The script never installs
   or overwrites a file automatically. The collector reloads coefficients on
   its next scan. Keep the output local; it contains your measured model IDs.

The script reads only per-request `type == "assistant"` / `message.usage`,
deduplicates request/message IDs, and ignores cumulative `result` totals. It uses
`message.model` (falling back to `usage.model`) just as the collector does. Rows
without recorded tokens, with cache creation, or without the chosen charge are
excluded. Fresh input is `input_tokens - cache_read_input_tokens`.

At least three independent token mixtures are needed to distinguish the three
prices. A model is omitted if the fit is underdetermined, produces invalid
prices, lacks validation samples, or exceeds 1% error on any training/validation
charge. The script reports how many models passed; missing models need more
varied measurements or a different calibration period. Do not combine periods
with different pricing/discounts. Optional observed windows come only from your
recorded input count and context ratio; no per-model windows are bundled.

This validates a linear **credits-price model**, not the accuracy of inferred
tokens when usage is hidden. Cache/output splitting and context-window inference
remain approximate. Recalibrate when pricing or routing changes; reported token
counts always take priority in the collector.

Synthetic checks: `python3 -m unittest discover -s scripts -p 'test_*.py'`.
