# Windows Media Foundation feasibility probe

This is a **test-only, unpackaged** Windows SDK executable. It asks whether the
runner's system Media Foundation Source Reader can open the existing pinned
`bunny.mp4` H.264 MP4 and return its first decoded frame as RGB32. It does not
build Iris, load Iris preview code, install codecs, or stage runtime libraries.
There are no new dependencies: compilation uses the hosted MSVC/Windows SDK and
its system import libraries; orchestration/tests use Python's standard library.

## Run and evidence

Request the `CI` workflow at the exact published revision. Its reusable
`windows-media-foundation-probe` job runs only on the explicit `workflow_dispatch`
caller; it is not part of normal PR CI or any release workflow. No artifacts are
uploaded. The runner image and its installed system codecs can change.

The driver builds in a fresh temporary directory, calls
`scripts/fetch-preview-corpus.py` with the existing manifest and the workflow's
**40,000,000-byte** budget, and passes only `corpus/bunny.mp4` to the executable.
The fetcher checks the pins for all five existing fixtures; other fixtures are
not opened by the probe. Fixture licenses/attribution remain in
[`tests/preview-corpus/fixtures.json`](../../tests/preview-corpus/fixtures.json).
No media bytes belong in git.

On success, one JSON log line records the fixture SHA-256, backend, codec,
visible RGB32 dimensions, coded output dimensions, visible offset, stride/buffer
length, read count, sample timestamp, RGB FNV-1a fingerprint and elapsed
milliseconds. FNV is a diagnostic, not a cryptographic or golden-image
assertion. The probe bounds native and coded output dimensions before decoding,
checks sample length before allocating a contiguous copy, checks locked-buffer
capacity/length, and fingerprints only the validated visible RGB bytes. Padding
and RGB32's undefined fourth byte are excluded. A black first frame is valid for
this sample's opening fade-in. Unexpected native type changes, end-of-stream
before a frame, invalid buffers and exhausted read limits fail.

The expected 854×480 input may have a larger coded RGB output only when both
`MF_MT_MINIMUM_DISPLAY_APERTURE` and `MF_MT_GEOMETRIC_APERTURE` are present, agree
on an integer rectangle fully inside the coded frame, and exactly match the
input dimensions. The minimum display aperture defines the valid image region;
pixels outside it are not displayed ([Microsoft documentation](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mt-minimum-display-aperture-attribute)).
The geometric aperture is separately checked for consistency
([Microsoft documentation](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mt-geometric-aperture-attribute)).
This recognizes the pinned run's 864×480 coded frame with a 854×480 visible
region; it does not tolerate a visible resize, synthesize a crop or prove that
Iris's renderer consumes the aperture. For a larger coded output, missing,
malformed, fractional, inconsistent or out-of-bounds apertures remain failures.
Source Reader failures retain a
fixed stage name and HRESULT (for example, `source_open` or `set_rgb32`) and
return a nonzero process/job exit status.

`elapsed_ms` on a native result is a single cold first-frame observation including
COM/MF startup, source opening, negotiation, decode, pixel validation and teardown;
it excludes compilation and fetching. Driver failures instead measure total
elapsed driver time and identify the failing phase. This is not a latency
benchmark, broad format/Windows compatibility evidence, licensing approval, PDF
support, product integration, packaging validation or release readiness.

Compiler/fetcher output is kept in disposable files, never echoed. The driver
validates the native JSON before logging it. Corpus, executable, object/debug
files and command output are removed before returning the result, on error, and
on subprocess timeout. Workflow `finally` and `always()` cleanup also cover an
interrupted driver; a forcibly destroyed hosted runner relies on runner disposal.
Frame limits bound the requested/returned output, **not all internal OS decoder
memory**; subprocess deadlines bound synchronous Source Reader calls that hang.

## Configuration and local checks

The workflow exposes these repository variables (the driver uses the same
positive-integer defaults). Windows ABI/format integer widths are intrinsic
bounds; policy below is configurable:

| Variable | Default | Meaning |
| --- | ---: | --- |
| `IRIS_MF_MAX_DIMENSION` | 2048 | Maximum width or height, native and RGB32 |
| `IRIS_MF_MAX_FRAME_BYTES` | 16777216 | Maximum RGB32 layout/sample/locked-buffer capacity |
| `IRIS_MF_MAX_READS` | 64 | Maximum Source Reader calls until a sample |
| `IRIS_MF_MAX_LOG_BYTES` | 4096 | Maximum child output read/accepted and retained result line |
| `IRIS_MF_BUILD_TIMEOUT_SECONDS` | 120 | Compiler deadline |
| `IRIS_MF_FETCH_TIMEOUT_SECONDS` | 300 | Whole corpus-fetch deadline |
| `IRIS_MF_DECODE_TIMEOUT_SECONDS` | 30 | Whole probe-process deadline |
| `IRIS_MF_JOB_TIMEOUT_MINUTES` | 10 | CI job deadline |

The workflow uses the existing corpus timeout/retry variables with defaults of
60 seconds per network operation, one transient retry and two seconds retry
delay. `IRIS_MF_TEMP_ROOT` names the workflow-owned temporary parent; local runs
may omit it to use the system temporary directory. For a local run from an x64
MSVC developer shell, set `IRIS_PREVIEW_CORPUS_MAX_BYTES=40000000` and invoke
`python prototype/windows-media-foundation/run.py`. That call downloads the
corpus; without an explicit budget the existing fetch helper fails closed.

Offline regression (no compiler, download or Windows needed):

```sh
python3 prototype/windows-media-foundation/test-probe.py
cargo fmt --all -- --check
```

The regression protects manual-only isolation, the budget/fetch/MP4-only wiring,
record validation, temporary-byte cleanup and timeout behavior. It does **not**
compile the SDK probe or establish that Windows can decode the sample. The manual
Windows job is required for that evidence; no successful native result is claimed
by adding this probe.
