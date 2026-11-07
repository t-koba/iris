#!/usr/bin/env python3
"""Guard the integration CI opt-in and preserved tag/manual triggers."""

from pathlib import Path

root = Path(__file__).resolve().parents[1]
ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
prototype = (root / ".github/workflows/windows-ffmpeg-prototype.yml").read_text(encoding="utf-8")

checks = {
    "CI mizu exclusion and all-tag filter": """  push:
    branches-ignore:
      - mizu
    tags:
      - '**'
""" in ci,
    "prototype mizu exclusion, all-tag filter, and path filters": """  push:
    branches-ignore:
      - mizu
    tags:
      - '**'
    paths:
""" in prototype,
    "CI manual, pull-request, and scheduled triggers": all(
        trigger in ci for trigger in ("  workflow_dispatch:\n", "  pull_request:\n", "  schedule:\n")
    ),
    "prototype manual and pull-request triggers": all(
        trigger in prototype for trigger in ("  workflow_dispatch: {}\n", "  pull_request:\n")
    ),
}
failed = [name for name, passed in checks.items() if not passed]
if failed:
    raise SystemExit("workflow trigger regression: " + "; ".join(failed))
print("workflow branch, tag, manual, pull-request, and schedule trigger checks passed")
