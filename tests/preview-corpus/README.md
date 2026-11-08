# Preview correctness corpus

`fixtures.json` pins five externally hosted assets (33,605,141 bytes total),
including source/license notes and exact HTTPS hosts/redirect destinations. The
assets are never stored in the repository or uploaded as CI artifacts.

## Fetch and run

From the repository root with Python 3 and the default-feature native dependencies:

```sh
parent=$(mktemp -d)
trap 'rm -rf "$parent"' EXIT
IRIS_PREVIEW_CORPUS_MAX_BYTES=40000000 python3 scripts/fetch-preview-corpus.py \
  --output "$parent/corpus"
IRIS_PREVIEW_LATENCY_WARMUPS=2 IRIS_PREVIEW_LATENCY_SAMPLES=21 \
  cargo run --release -p iris-thumb-preview --example preview_corpus_smoke \
  --locked -- "$parent/corpus"
```

The fetch helper requires `IRIS_PREVIEW_CORPUS_MAX_BYTES`, validates the fixture
set and total budget before network access, checks HTTPS redirect policy, streams
under per-file size bounds, and verifies each pinned size and SHA-256. The default
per-operation network timeout is 30 seconds (CI selects 60 seconds). Direct calls
default to no retries; CI defaults to one transient-failure retry with a two-second
delay. `IRIS_PREVIEW_CORPUS_RETRIES` and
`IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS` configure that policy. Policy, status,
size and digest failures are not retried. Downloads use private partial files and
atomic installation. The output directory must not exist; on failure, the helper
removes the directory it created. The caller removes successful output.

## Correctness and measurements

The smoke requires the product-default `video`, `pdf` and `raw-full` features. It
checks a high-resolution JPEG resize, small JPEG passthrough, CR3 embedded preview,
H.264 video decode, and one-page PDF render/page count. Outputs must be decodable,
bounded JPEGs; photo, RAW and PDF outputs must contain nonuniform pixels. The
pinned video's first frame is a valid black fade-in, so its check validates the
decoder path and dimensions rather than rejecting that frame as blank. RAW sensor
data is not demosaiced.

Before extraction, all five inputs are streamed through a fixed 64 KiB buffer and
checked against their pinned sizes and hashes. The smoke emits one
`PREVIEW_CORRECTNESS` and four `PREVIEW_LATENCY` JSON records. It measures JPEG,
CR3, H.264 and PDF extraction, with configured warmups and sample counts plus
min/median/p90/median-absolute-deviation/max nanoseconds. Each record includes the
fixture digest, source/run, Cargo.lock, compiler, target/features, runner and CPU
identity. PDF timings explicitly identify CoreGraphics on macOS or dynamically
loaded PDFium on Linux. Every PDF warmup and measured JPEG is decoded to RGB and
compared with the correctness baseline after timing; the baseline pixel digest
binds the correctness and timing records.

These are crate-level local-file diagnostics, not UI/end-to-end latency. PDF
renderer timings are kept separate by platform and are not an FFmpeg effect or
cross-platform comparison. The measurements have no performance threshold and do
not establish legal approval, PDFium attestation, shipped-runtime provenance or
release readiness. Sequential samples may reflect cache/order effects.

## Report-only runtime validation

The report-only macOS runtime validation fetches the corpus once and runs the same
smoke against separate Homebrew-baseline and pinned-candidate FFmpeg build targets.
Its bounded JSON report retains correctness and timing records; paired median
differences remain limited to JPEG, CR3 and video. PDF timing is not paired because
its renderer is a macOS system framework, not FFmpeg. Corpus, app and runtime bytes
are temporary; only bounded JSON reports are uploaded under the existing
`IRIS_FFMPEG_MAX_REPORT_BYTES` cap (266,240 bytes by default).

The report-only Linux FFmpeg validation runs the default-feature smoke from the
staged AppDir against its bundled FFmpeg and PDFium runtimes. Its existing
`linux-ffmpeg.json` includes validated corpus records and the PDFium version plus
asset/library hashes under the same report cap. The workflow sets
`IRIS_VERIFY_PDFIUM_PROVENANCE=0`; the report explicitly records that attestation
was not attempted and does not claim provenance verification. It uploads JSON
only, never corpus, package, AppDir, source or runtime bytes. Other correctness CI
runs retain no corpus report or fixture artifacts.

Corpus budgets/timeouts/retries and latency iterations are configured through the
corresponding workflow variables; report and command output/deadline caps are also
enforced. Linux PDFium is found beside the staged AppDir executable (under
`usr/lib/iris`); macOS renders through CoreGraphics. These report-only checks do not
change release runtime selection or packaging.

Offline contracts: `python3 scripts/test-fetch-preview-corpus.py`,
`python3 scripts/test-macos-ffmpeg.py`, `python3 scripts/test-linux-ffmpeg.py`, and
`cargo test -p iris-thumb-preview --example preview_corpus_smoke --locked --offline`
(with default-feature native build dependencies).
