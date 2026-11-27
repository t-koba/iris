#!/usr/bin/env python3
"""Shared product build profile for packaging, parity production and acceptance.

The JSON file is the single source of truth. This reader fails closed on an
unknown platform or a malformed profile; it never builds or changes state.
"""

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "scripts" / "product-build-profile.json"
PLATFORMS = ("linux", "macos", "windows")
LEVELS = ("0", "1", "2", "3", "s", "z")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def load(path=POLICY, max_bytes=65536):
    data = Path(path).read_bytes()
    require(len(data) <= max_bytes, "product build profile exceeds byte budget")
    policy = json.loads(data.decode("utf-8"))
    require(type(policy) is dict and set(policy) == {"schema", "platforms"},
            "invalid product build profile shape")
    require(type(policy["schema"]) is int and policy["schema"] == 1,
            "unsupported product build profile schema")
    platforms = policy["platforms"]
    require(type(platforms) is dict and set(platforms) == set(PLATFORMS),
            "invalid product build profile platforms")
    for platform, spec in platforms.items():
        require(type(spec) is dict and set(spec) == {"build_profile", "release_opt_level"},
                f"invalid {platform} product build profile")
        require(spec["build_profile"] == "release"
                and type(spec["release_opt_level"]) is str
                and spec["release_opt_level"] in LEVELS,
                f"invalid {platform} product release settings")
    return platforms


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=PLATFORMS, required=True)
    parser.add_argument("--policy", type=Path, default=POLICY)
    args = parser.parse_args()
    try:
        print(load(args.policy)[args.platform]["release_opt_level"])
    except (OSError, UnicodeDecodeError, ValueError, json.JSONDecodeError) as error:
        parser.exit(1, f"product build profile rejected: {error}\n")


if __name__ == "__main__":
    main()
