# Preview correctness corpus

`fixtures.json` pins six fixtures (38,559,104 bytes total): four externally hosted
assets with source/license notes and exact HTTPS hosts/redirect destinations, and
two locally generated JPEGs. A deterministic DC-only baseline encoder generates
`landscape.jpg` at 4000x3000 and `sample.jpg` at 200x200 without contacting external
hosts. Their exact bytes are pinned by size and SHA-256, just like the external assets.
External assets and generated fixture bytes are never stored in the repository or
uploaded as CI artifacts.

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
default to no retries. The CI and validation workflows default to four retries for
transient failures; a numeric `Retry-After` on HTTP 429 or 503 raises that wait up
to the configured cap. Retries otherwise back off exponentially from
`IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS` (default two seconds), capped by
`IRIS_PREVIEW_CORPUS_MAX_RETRY_DELAY_SECONDS` (default 30 seconds).
`IRIS_PREVIEW_CORPUS_RETRIES` sets the retry count; `release.yml` still defaults to
one retry. Policy, status, size and digest failures are not retried. Downloads and
generated fixtures use private partial files and atomic installation. The output
directory must not exist; on failure, the helper removes the directory it created.
The caller removes successful output.

## Correctness and measurements

The smoke requires the product-default `video`, `pdf` and `raw-full` features. It
checks resize of the 4000x3000 JPEG, small JPEG passthrough, CR3 embedded preview,
H.264 and H.265/HEVC video decode, and one-page PDF render/page count. Outputs must
be decodable, bounded JPEGs; photo, RAW and PDF outputs must contain nonuniform
pixels. First-frame uniformity is not established for either pinned video, so their
checks validate the decoder path and dimensions rather than rejecting a valid blank
opening frame. RAW sensor data is not demosaiced.

Before extraction, all six inputs are streamed through a fixed 64 KiB buffer and
checked against their pinned sizes and hashes. The smoke emits one
`PREVIEW_CORRECTNESS` and five `PREVIEW_LATENCY` JSON records. It measures JPEG,
CR3, H.264, HEVC and PDF extraction, with configured warmups and sample counts plus
min/median/p90/median-absolute-deviation/max nanoseconds. Each record includes the
fixture digest, source/run, Cargo.lock, compiler, target/features, runner and CPU
identity; `build_profile` and `release_opt_level` come from compile-time values
emitted by `crates/thumb-preview/build.rs` from Cargo's `PROFILE` and `OPT_LEVEL`,
not from runtime environment variables. PDF timings explicitly identify CoreGraphics on macOS or dynamically
loaded PDFium on Linux. Every PDF warmup and measured JPEG is decoded to RGB and
compared with the correctness baseline after timing; the baseline pixel digest
binds the correctness and timing records.

These are crate-level local-file diagnostics, not UI/end-to-end latency. PDF
renderer timings are kept separate by platform and are not an FFmpeg effect or
cross-platform comparison. The measurements have no performance threshold and do
not establish legal approval, PDFium attestation, shipped-runtime provenance or
release readiness. Sequential samples may reflect cache/order effects.

## Windows

The Windows native-preview validation workflow retains the full structured
`PREVIEW_CORRECTNESS` and `PREVIEW_LATENCY` records from the staged default-feature
smoke in `preview-smoke.json` under `preview_records`, alongside `record_count`
and a bounded 2,000-character `output_tail` diagnostic; the serialized record is
capped at 200,000 characters. A successful run must contain exactly six records
(one correctness and five latency); malformed or missing records fail the workflow,
and a failing smoke still writes its diagnostic record before the step throws. These
remain crate-level diagnostics for the staged Windows runtime, not UI parity,
platform comparison, release approval or legal/PDFium provenance verification.

## Report-only runtime validation

The report-only macOS runtime validation fetches the corpus once and runs the same
smoke against separate Homebrew-baseline and pinned-candidate FFmpeg build targets.
Its bounded JSON report retains correctness and timing records; paired median
differences remain limited to JPEG, CR3 and both video codecs. PDF timing is not
paired because its renderer is a macOS system framework, not FFmpeg. Corpus, app
and runtime bytes are temporary; only bounded JSON reports are uploaded under the existing
`IRIS_FFMPEG_MAX_REPORT_BYTES` cap (266,240 bytes by default).

The report's `control_drift` field records absolute median drift for the non-FFmpeg JPEG and
CR3 controls in these sequential diagnostics. It defaults to a 10% limit, overridable with
`IRIS_PREVIEW_CONTROL_DRIFT_PERCENT`. This is a validity diagnostic for whether control drift
prevents attributing FFmpeg deltas, not a performance threshold, gate or parity claim.

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

## Parity status

No retained evidence establishes preview feature or latency parity with a previously
shipped default-feature product. As checked on 2026-11-23, the public GitHub API
reported no releases or tags for `t-koba/iris`; that leaves no public tagged release
baseline to recover, but does not establish that no builds were distributed by other
means. The macOS paired diagnostic identifies its baseline as an existing Homebrew
FFmpeg selection, not an approved compatible runtime. Linux and Windows retain
single-runtime staged diagnostics without a paired latency baseline. The corpus
exercises embedded RAW previews rather than demosaiced sensor data and measures
crate-level extraction rather than UI latency. A same-platform baseline/candidate
pair with exact source, runtime, input and hardware identity is required before any
parity conclusion; until then parity remains unknown and the all-platform release
hold stays in force.

A defensible rebuilt reference is available in source history even though no shipped
baseline was found. Commit `12fb51a0ba07c299ea4e9b08890a895797148e11`
(2026-11-23, "Reload visual preview when Quick Look opens from Properties") is the
parent of the pure-Rust JPEG 2000 migration `7d0b311`. It pins the same
`tests/preview-corpus/fixtures.json` bytes (SHA-256
`349e9b98e5a959bf0915493e631b58e8927dd574d31349a97031fc3cdf635dd1`) and the same
fetch script bytes (SHA-256
`8ad5eab2099d7bd7358d3b9e1448a330fe7c271a6efb4c3212270a9347d8d2b3`); its
preview smoke predates the v2 identity schema (`iris.preview_identity.v2`) and
the later `PREVIEW_PDFIUM_RUNTIME` attribution line. The paired-source verifier
therefore stages the candidate's corpus smoke, J2K smoke, fixture generator and
preview `build.rs` into both detached worktrees; both sides emit v2 identities, so `paired_latency`
and `paired_j2k` are produced on the same runner with the same harness and native
runtime recorded in each pair. Those medians are a legitimate same-runner source
comparison of the jpeg2k migration, not proof of shipped-release parity. The
release gate consumes them only when all three platform reports pass identity
binding, exact SHA/source checks and no-slowdown checks. This diagnostic makes no
parity claim by itself, and the release hold remains until that gate passes.
Its `Cargo.lock` resolves `jpeg2k 0.10.1` with `openjpeg-sys` instead of the
candidate's pure-Rust `jpeg2k 0.11.0`. That makes it a candidate reference for a
paired, same-runner comparison of the pinned corpus, not parity evidence by itself:
it was never a shipped release, its dependencies are not in the local offline
cache, and no paired coverage or latency record from that revision has been
retained. The paired run must stage the same native runtimes and
fixtures for both revisions, keep the exact source and runner identities in the
report, and preserve the release hold until every platform and media type is
accounted for.

The six pinned fixtures still contain no JPEG 2000 input (`jp2`, `j2k` or
`jpx`), and `fixtures.json` is unchanged. The separate overlay is generated,
never stored in the repository. `examples/generate_j2k_fixture.rs` builds a
256x256 RGB image with `Rgb([x as u8, y as u8, 128])`, encodes it with the
explicit `Iris paired J2K fixture v1` COM marker, and fails closed unless the
JP2 is exactly 393 bytes with SHA-256
`e44d3480025d87433287c64775c0486cc2e6f7369c6df5b8a249d89f6d2a2caa`; the output
file is opened with `create_new`. The `gray-1x1.jp2` file under
`crates/thumb-preview/tests/fixtures` remains a unit-test asset outside this
overlay.

`examples/j2k_corpus_smoke.rs` is shared by both source revisions through
`image_generic::extract_image_thumbnail_from_bytes` (128-pixel limit, JPEG
preference). It verifies the pin, requires nonuniform 128x128 JPEG output, and
emits one `PREVIEW_CORRECTNESS` and one `PREVIEW_LATENCY` record with the input
pin, output dimensions/bytes/decoded RGB digest, timing statistics and
source/toolchain/runner identity. The opt-in paired-source report stages
detached worktrees for `12fb51a0ba07c299ea4e9b08890a895797148e11` and an exact
candidate SHA, stages the candidate examples and `build.rs` into both worktrees
as untracked files, generates the JP2 once from the candidate generator, and runs both
revisions' pinned corpus and J2K smokes on the same inputs:

```sh
python3 scripts/parity_source_compare.py \
  --candidate <full-commit-sha> --corpus "$parent/corpus" \
  --output "$parent/parity-source.json" --warmups 2 --samples 21
```

The bounded JSON (at most 266,240 bytes) retains the full records, source and
`Cargo.lock` digests, harness/fixture pins, `build_script_sha256`, paired median
deltas and a fail-closed input/operation/identity/output comparison. The release
verifier recomputes that build-script digest from the repository and rejects a
report whose value differs; the build script itself fails closed if RUSTFLAGS
carries an `opt-level` codegen override that `OPT_LEVEL` cannot see. Baseline dependencies
may require network unless the caller sets `CARGO_NET_OFFLINE` and both
dependency sets are already cached; worktrees are removed even on failure. This
is report-only: it makes no parity claim, measures crate-level extraction rather
than UI latency, makes no parity claim by itself, and leaves the release hold in
force.

`scripts/parity_source_compare.py` remains report-only and never satisfies a release
prerequisite on its own. Publication uses the separate `scripts/release-parity.py`
verifier, which requires all three same-platform linux/macos/windows pairs, zero
unpaired sections, exact source/revision/harness binding, PDFium library attribution
on Linux and Windows, and no candidate median slowdown. FFmpeg-bearing identities
(corpus correctness and every `uses_ffmpeg` timing) additionally record codec ABI,
configure flags and license; the verifier derives the expected values from
`packaging/linux/ffmpeg.json` and rejects a pair even when both sides used the same
system runtime. The paired evidence workflow builds that pinned runtime on Linux
(`scripts/build-linux-ffmpeg.sh` plus `linux-ffmpeg.py validate`) and macOS
(`macos-ffmpeg.py prepare`) and binds both worktrees to it with `FFMPEG_DIR`,
pkg-config paths and the matching dynamic-library search path; Windows keeps its
pinned vcpkg overlay. A shared-v2 report produced by the pinned producer can
discharge that gate only when all those conditions hold; an unpaired diagnostic
records why parity remains unknown instead. None of this is a release or parity
claim by itself, and release readiness remains undecided.

Offline contracts: `python3 scripts/test-fetch-preview-corpus.py`,
`python3 scripts/test-parity-source-compare.py`,
`python3 scripts/test-macos-ffmpeg.py`, `python3 scripts/test-linux-ffmpeg.py`, and
`cargo test -p iris-thumb-preview --example preview_corpus_smoke --locked --offline`
(with default-feature native build dependencies).
