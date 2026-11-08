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
operation (CI chooses 60); urllib also rejects redirect loops. Direct invocations
default to zero retries. CI/release workflows default to one retry after transient
HTTP/network failures, with a two-second delay; repository variables
`IRIS_PREVIEW_CORPUS_RETRIES` and `IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS` configure
those values. The retry policy never retries redirect/policy, status, size or digest
validation failures, and each attempt uses a new private partial file before the
verified fixture is atomically installed. The output directory must not exist;
existing files and symlinks are never reused. Failed downloads remove the directory
the helper created. After success, the caller owns removal; CI always removes its
temporary parent, including on smoke failure.

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

Before extraction, the smoke streams all five inputs through a fixed 64 KiB
buffer and verifies their pinned sizes and hashes. After correctness checks it
emits one `PREVIEW_CORRECTNESS` and three `PREVIEW_LATENCY` JSON records to stdout.
The correctness record binds every fixture to its checked output dimensions,
format and extraction source. Latency records measure JPEG resize, CR3 embedded
preview extraction, and **FFmpeg-dependent H.264 video extraction**. `uses_ffmpeg`
is false for JPEG/CR3: those operations use Iris Rust code, not FFmpeg or PDFium.
PDF correctness is checked, but PDF latency is not measured.

Required settings `IRIS_PREVIEW_LATENCY_WARMUPS` and `IRIS_PREVIEW_LATENCY_SAMPLES`
bound the diagnostic work; CI defaults to two warmups and 21 measurements. Each
latency record reports min/median/p90/median absolute deviation/max nanoseconds,
source/run identifiers when supplied by CI, compiler/target, runner/CPU identity,
release optimization level, Cargo.lock digest, features, and fixture digest.
Timings cover crate-level local-file extraction, not UI/end-to-end latency;
CR3 measures embedded-preview extraction, not sensor demosaicing.

The report-only macOS runtime-validation driver fetches this corpus **once** with
the allowlisted, size-bounded helper. It builds default-feature smoke executables
in separate fresh Cargo targets for Homebrew FFmpeg and the pinned prefix. The
candidate smoke runs against its rewritten, signed and verified staged native
closure. Both runs use the same corpus directory and verify it before measurement.
Its existing `macos-candidate.json` retains both correctness/timing sets and paired
median differences only when fixture hashes/sizes, operations, output dimensions,
format/source, Cargo lock/features, build/compiler, runner/CPU and source identities
match. Any mismatch rejects pairing. Samples/warmups must also match. Corpus
fetch or Homebrew baseline failure records a bounded `unavailable` reason without
paired comparisons; candidate native inventory and generated MPEG-4/PNG smokes
still run. Candidate correctness failures remain fatal, not optional diagnostics.

The driver keeps the existing `IRIS_FFMPEG_MAX_REPORT_BYTES` cap (266,240 bytes)
and command-output/deadline bounds. The workflow configures the corpus budget
(default 40,000,000 bytes), network timeout (60 seconds), and latency iterations
through the corresponding repository variables. Corpus, build targets, native
sources, logs and temporary apps are removed on completion or failure; TERM/INT
unwind active commands and owned scratch, with workflow cleanup as a backstop.
Only the two existing audit JSON artifacts are retained, never fixtures or packages.
Other CI corpus runs retain no fixture/report artifact.

These are **diagnostics, not latency gates, parity evidence or legal approval**.
There is no performance threshold; sequential runs may reflect cache/order effects.
No production runtime or release packaging selection changes. Native macOS results
must still be collected on the exact source; Windows default-preview evidence
remains separate work.

Offline contracts: `python3 scripts/test-fetch-preview-corpus.py` and
`python3 scripts/test-macos-ffmpeg.py`.
