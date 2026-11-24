#!/usr/bin/env python3
"""Bounded offline regression for the dependency audit's RSA advisory guard."""

import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/audit-dependencies.sh"
TREE_ARGS = ["tree", "--workspace", "--locked", "--offline", "-e", "features", "-i", "rsa"]
TREE_CALL = ["cargo", *TREE_ARGS]
ABSENCE = (
    "error: package ID specification `rsa` did not match any packages\n"
    "\n"
    "help: a package with a similar name exists: `ash`\n"
)
LOCKS = ["Cargo.lock", "crates/fuzz/Cargo.lock", "prototype/win/Cargo.lock"]
ROOT_AUDIT = ["cargo", "audit", "--file", "Cargo.lock", "--ignore", "RUSTSEC-2023-0071"]
OTHER_AUDITS = [["cargo", "audit", "--file", lock] for lock in LOCKS[1:]]

CARGO_SHIM = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
with Path(os.environ["SHIM_LOG"]).open("a", encoding="utf-8") as stream:
    stream.write(json.dumps([Path(sys.argv[0]).name, *sys.argv[1:]]) + "\\n")
if sys.argv[1:2] == ["tree"]:
    sys.stderr.write(os.environ.get("SHIM_TREE_OUTPUT", ""))
    raise SystemExit(int(os.environ.get("SHIM_TREE_STATUS", "1")))
if sys.argv[1:2] == ["audit"]:
    raise SystemExit(0)
raise SystemExit(99)
'''

GIT_SHIM = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
with Path(os.environ["SHIM_LOG"]).open("a", encoding="utf-8") as stream:
    stream.write(json.dumps([Path(sys.argv[0]).name, *sys.argv[1:]]) + "\\n")
if sys.argv[1:3] == ["ls-files", "*Cargo.lock"]:
    sys.stdout.write(os.environ.get("SHIM_LOCKS", ""))
    raise SystemExit(0)
raise SystemExit(99)
'''


def install_shim(directory, name, source):
    path = directory / name
    path.write_text(source, encoding="utf-8")
    path.chmod(0o755)


def run_case(label, output, status, *, succeeds, audits, diagnostics=()):
    with tempfile.TemporaryDirectory() as temporary:
        work = Path(temporary)
        bin_dir = work / "bin"
        bin_dir.mkdir()
        install_shim(bin_dir, "cargo", CARGO_SHIM)
        install_shim(bin_dir, "git", GIT_SHIM)
        for lock in LOCKS:
            path = work / lock
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("# mock lockfile\n", encoding="utf-8")
        log = work / "calls.jsonl"
        env = dict(os.environ)
        env.update(
            PATH=os.pathsep.join([str(bin_dir), env.get("PATH", "")]),
            SHIM_LOG=str(log),
            SHIM_TREE_OUTPUT=output,
            SHIM_TREE_STATUS=str(status),
            SHIM_LOCKS="\n".join(LOCKS) + "\n",
        )
        result = subprocess.run(
            ["bash", str(SCRIPT)], cwd=work, env=env, encoding="utf-8",
            capture_output=True, text=True, timeout=20,
        )
        calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()] if log.exists() else []

        def require(ok, message):
            if not ok:
                raise AssertionError(
                    f"{label}: {message}\nstdout={result.stdout!r}\nstderr={result.stderr!r}\ncalls={calls!r}"
                )

        def calls_for(program, subcommand=None):
            return [call for call in calls if call[:1] == [program]
                    and (subcommand is None or call[1:2] == [subcommand])]

        require(result.returncode == (0 if succeeds else 1), "unexpected exit status")
        require(calls_for("cargo", "tree") == [TREE_CALL], "tree query mismatch")
        require(calls_for("cargo", "audit") == audits, "audit invocation mismatch")
        if succeeds:
            require(calls_for("git") == [["git", "ls-files", "*Cargo.lock"]], "lockfile discovery mismatch")
            for lock in LOCKS:
                require(f"auditing {lock}" in result.stdout, f"missing audit line for {lock}")
        else:
            for diagnostic in diagnostics:
                require(diagnostic in result.stderr, f"missing diagnostic {diagnostic!r}")


def main():
    run_case(
        "rsa present", "rsa v0.9.6\n", 0, succeeds=False, audits=[],
        diagnostics=("the vulnerable rsa crate is enabled in the default workspace graph", "rsa v0.9.6"),
    )
    run_case(
        "exact absence", ABSENCE, 101, succeeds=True,
        audits=[ROOT_AUDIT, *OTHER_AUDITS],
    )
    run_case(
        "resolution error",
        'error: failed to select a version for the requirement `rsa = "0.9"`\n'
        "Caused by:\n  no matching package named `rsa` found\n",
        101, succeeds=False, audits=[],
        diagnostics=("cargo tree could not confirm that rsa is absent", "failed to select a version", "Caused by:"),
    )
    run_case(
        "mixed absence and another error", ABSENCE + "error: failed to resolve workspace dependencies\n",
        101, succeeds=False, audits=[],
        diagnostics=("cargo tree could not confirm that rsa is absent", "failed to resolve workspace dependencies"),
    )
    run_case(
        "absence not on the first line", "warning: stale lockfile\n" + ABSENCE,
        101, succeeds=False, audits=[],
        diagnostics=("cargo tree could not confirm that rsa is absent", "warning: stale lockfile"),
    )
    print("dependency audit guard regressions passed")


if __name__ == "__main__":
    main()
