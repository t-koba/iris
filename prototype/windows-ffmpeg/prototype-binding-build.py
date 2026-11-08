"""Transactional source override for the test-only default-feature Windows app."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tomllib

NAME = "ffmpeg-sys-next"
REPOSITORY = "https://github.com/zmwangx/rust-ffmpeg-sys"
COMMIT = "5aa9dc312e0ab9b762bae7ca367d17761d0c9447"
VERSION = "9.1.0"
SOURCE = f"git+{REPOSITORY}?rev={COMMIT}#{COMMIT}"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
CHECKSUM = "c6f932d8c1cede016c65d4f1435f323369c99c74df55143b18676aa10e7603f9"
CONFIG = ["--config", f'patch.crates-io.{NAME}.git="{REPOSITORY}"',
          "--config", f'patch.crates-io.{NAME}.rev="{COMMIT}"']


def validate_pin(pin):
    if (pin["repository"], pin["commit"], pin["version"]) != (REPOSITORY, COMMIT, VERSION):
        raise ValueError("prototype binding pin drifted")


def lock_text(raw):
    # Keep lock identity independent of checkout line endings; callers still
    # retain and restore the original bytes exactly.
    return re.sub(r"\r+\n|\r", "\n", raw.decode("utf-8-sig"))


def parse_lock(raw):
    return tomllib.loads(lock_text(raw))


def validate_lock(original, temporary):
    before = parse_lock(original)
    after = parse_lock(temporary)
    old = [p for p in before["package"] if p["name"] == NAME]
    new = [p for p in after["package"] if p["name"] == NAME]
    if len(old) != 1 or len(new) != 1:
        raise ValueError("expected exactly one binding package before and after override")
    old, new = old[0], new[0]
    if (old["version"], old["source"], old["checksum"]) != ("9.0.1", REGISTRY, CHECKSUM):
        raise ValueError("production binding identity drifted")
    expected = {**old, "version": VERSION, "source": SOURCE}
    del expected["checksum"]
    if new != expected:
        raise ValueError("temporary binding identity or dependencies drifted")
    # Order is immaterial; all package records, dependency edges and lock metadata
    # other than this one source/version/checksum transition must be unchanged.
    before["package"] = sorted(before["package"], key=lambda p: (p["name"], p["version"], p.get("source", "")))
    after["package"] = sorted([old if p["name"] == NAME else p for p in after["package"]],
                              key=lambda p: (p["name"], p["version"], p.get("source", "")))
    if before != after:
        raise ValueError("unrelated Cargo.lock drift; refusing prototype build")


def override_lock(original):
    # Change only the pinned binding record; `cargo update` can re-resolve
    # unrelated transitive dependencies, which the prototype must refuse.
    before = parse_lock(original)
    packages = [p for p in before["package"] if p["name"] == NAME]
    if len(packages) != 1:
        raise ValueError("expected exactly one production binding package")
    package = packages[0]
    if (package["version"], package["source"], package["checksum"]) != ("9.0.1", REGISTRY, CHECKSUM):
        raise ValueError("production binding identity drifted")

    text = lock_text(original)
    names = list(re.finditer(
        rf'(?m)^[ \t]*name[ \t]*=[ \t]*{re.escape(json.dumps(NAME))}[ \t]*$', text
    ))
    if len(names) != 1:
        raise ValueError("could not locate unique production binding package table")
    start = text.rfind("[[package]]", 0, names[0].start())
    end = text.find("[[package]]", names[0].end())
    if start < 0:
        raise ValueError("could not locate production binding package table")
    if end < 0:
        end = len(text)
    section = text[start:end]

    def replace_field(key, old, new):
        nonlocal section
        pattern = re.compile(
            rf'(?m)^(?P<prefix>[ \t]*{re.escape(key)}[ \t]*=[ \t]*)'
            rf'{re.escape(json.dumps(old))}[ \t]*$'
        )
        section, count = pattern.subn(
            lambda match: match.group("prefix") + json.dumps(new), section, count=1
        )
        if count != 1:
            raise ValueError(f"expected one pinned {key} field in binding package table")

    replace_field("version", "9.0.1", VERSION)
    replace_field("source", REGISTRY, SOURCE)
    checksum = re.compile(
        rf'(?m)^[ \t]*checksum[ \t]*=[ \t]*{re.escape(json.dumps(CHECKSUM))}[ \t]*(?:\n|$)'
    )
    section, count = checksum.subn("", section, count=1)
    if count != 1:
        raise ValueError("expected one pinned checksum field in binding package table")
    return (text[:start] + section + text[end:]).encode("utf-8")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def build(root, pin, evidence):
    lock = root / "Cargo.lock"
    original = lock.read_bytes()  # Memory only: never leave a backup or changed lock behind.
    record = {
        "schema": 1, "package": NAME, "repository": REPOSITORY,
        "commit": COMMIT, "version": VERSION, "source": SOURCE,
        "note": "Test-only pinned, unsigned upstream commit; no signature claim or release approval.",
        "original_lock_sha256": digest(original), "status": "started", "lock_restored": False,
    }
    evidence.parent.mkdir(parents=True, exist_ok=True)
    # Invalidate any stale success before Cargo can fail.
    evidence.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    try:
        validate_pin(pin)
        temporary = override_lock(original)
        validate_lock(original, temporary)
        lock.write_bytes(temporary)
        record["temporary_lock_sha256"] = digest(temporary)
        record["status"] = "lock_validated"
        # `--locked` verifies the precise source override without re-resolving
        # unrelated dependencies. No feature flags keep product defaults enabled;
        # CLI config does not persist into manifests or Cargo configuration.
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "iris-ui-shell",
                        "--bin", "Iris", *CONFIG], cwd=root, check=True)
        if lock.read_bytes() != temporary:
            raise ValueError("locked build changed Cargo.lock")
        record["status"] = "build_passed"
    except BaseException as error:
        record["status"] = "failed"
        record["error_type"] = type(error).__name__
        raise
    finally:
        # Includes lock preparation/validation, build failures and interruption.
        # Abrupt process termination cannot execute finally; use an isolated CI checkout.
        lock.write_bytes(original)
        record["lock_restored"] = lock.read_bytes() == original
        record["restored_lock_sha256"] = digest(lock.read_bytes())
        evidence.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        if not record["lock_restored"]:
            raise RuntimeError("production lock restoration failed")


def main():
    here = Path(__file__).resolve().parent
    pins = json.loads((here / "pins.json").read_text(encoding="utf-8-sig"))
    root = here.parents[1]
    build(root, pins["ffmpeg_sys_next"], root / "target/prototype-windows-ffmpeg/binding-override.json")


if __name__ == "__main__":
    main()
