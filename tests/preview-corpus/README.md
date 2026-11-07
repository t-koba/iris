# Preview correctness corpus

`fixtures.json` pins five externally hosted assets (33,605,141 bytes total).
Its `notes` retain each source and license attribution; filenames are local
single basenames. `allowed_hosts` is an exact HTTPS allowlist, including the
Wikimedia and GitHub release redirect destinations. No corpus bytes belong in
the repository, Cargo cache, or uploaded CI artifacts.

From the repository root, with Python 3 and default-feature native dependencies:

```sh
parent=$(mktemp -d)
trap 'rm -rf "$parent"' EXIT
IRIS_PREVIEW_CORPUS_MAX_BYTES=40000000 python3 scripts/fetch-preview-corpus.py \
  --output "$parent/corpus"
IRIS_PREVIEW_LATENCY_WARMUPS=2 IRIS_PREVIEW_LATENCY_SAMPLES=21 \
  cargo run --release -p iris-thumb-preview --example preview_corpus_smoke \
  --locked -- "$parent/corpus"
```

The helper requires `IRIS_PREVIEW_CORPUS_MAX_BYTES` (no implicit download budget).
It validates all entries and the total before network access, checks each HTTPS
redirect, streams with a per-file size bound, and verifies actual size and SHA-256.
`IRIS_PREVIEW_CORPUS_TIMEOUT_SECONDS` defaults to 30 seconds per blocking network
operation (CI chooses 60); urllib also rejects redirect loops. The output directory
must not exist; existing files and symlinks are never reused. Failed downloads
remove the directory the helper created. After success, the caller owns removal;
CI always removes its temporary parent, including on smoke failure.

The smoke requires `video`, `pdf`, and `raw-full`, all enabled by default. It
checks the 4000x3000 JPEG resize, small JPEG passthrough, structured CR3 embedded
JPEG preview, H.264 MP4 thumbnail, and one-page PDF render/count. Outputs must be
decodable JPEGs with valid bounded dimensions; photo, CR3 and PDF outputs must
contain nonuniform pixels. The first frame of this pinned video is a black fade-in,
so the video check validates dimensions and the decoder path instead of rejecting
that valid frame as blank. RAW sensor data is not demosaiced.
On Linux, PDFium must be discoverable by the product loader: CI runs from the
existing pinned AppDir's `usr/lib/iris` directory, using an absolute Cargo manifest
path. macOS uses Core Graphics. FFmpeg is linked from each CI host's normal system
runtime (Ubuntu packages or Homebrew); this is decoder correctness only, not
shipped-runtime provenance, licensing, or package-runtime evidence. This does not
change release packaging. For a Linux product-profile match, set
`CARGO_PROFILE_RELEASE_OPT_LEVEL=s`; CI uses that setting on Ubuntu and the workspace
default optimization level on macOS.

After correctness checks, the smoke verifies the pinned JPEG and CR3 bytes and emits
exactly two `PREVIEW_LATENCY` JSON records to stdout. Required environment settings
`IRIS_PREVIEW_LATENCY_WARMUPS` and `IRIS_PREVIEW_LATENCY_SAMPLES` bound the diagnostic
work; CI configures two warmups and 21 measurements. Each record reports
min/median/p90/median absolute deviation/max nanoseconds and
includes source/run identifiers when supplied by CI, verbose Rust compiler/target,
runner/CPU identity, release optimization level, Cargo.lock digest, feature flags,
and fixture digest. Timings cover crate-level local-file extraction, not UI/end-to-end
latency; CR3 measures embedded-preview extraction, not sensor demosaicing. The
iteration counts are explicit environment configuration; fixture verification uses a
fixed 64 KiB streaming buffer. No report or fixture is uploaded as an artifact. These are
**diagnostics, not latency gates or parity evidence**: they are not matched against a
comparable baseline or candidate, and do not represent video, PDF, or Windows latency.
Native comparable baseline/candidate measurements and Windows default-preview evidence
remain separate work. This smoke is not license approval.

Offline fetch-helper regressions: `python3 scripts/test-fetch-preview-corpus.py`.
