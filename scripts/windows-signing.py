#!/usr/bin/env python3
"""Fail-closed tag-release Authenticode signing and verification for Windows.

`plan` emits the exact signtool sign argv (including the password) and its
value provenance; it is a secret-bearing artifact and is never persisted here.
`sign` provisions a short-lived private copy of the PFX, signs, verifies the
RFC 3161 timestamp and writes scrubbed evidence bound to the signed bytes.
`verify` runs the same check on an already staged installer. All tool output is
bounded and scrubbed of every secret value before it reaches evidence or logs.
"""
import argparse
import getpass
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile

SCHEMA = 1
MAX_TOOL_BYTES = 32768
MAX_TEXT_CHARS = 8192
MAX_PATH_CHARS = 4096
MAX_PFX_BYTES = 1024 * 1024
TOOL_TIMEOUT_SECONDS = 30

PFX_ENV = "IRIS_WINDOWS_SIGN_PFX"
PASSWORD_ENV = "IRIS_WINDOWS_SIGN_PFX_PASSWORD"
TIMESTAMP_ENV = "IRIS_WINDOWS_SIGN_TIMESTAMP_URL"
FILE_ENV = "IRIS_WINDOWS_SIGN_FILE"
SIGNTOOL_ENV = "IRIS_SIGNTOOL"

SUCCESS_LINE = re.compile(r"^Successfully verified:\s*\S", re.IGNORECASE)
SUCCESS_COUNT = re.compile(r"^Number of files successfully Verified:\s*([0-9]+)$",
                           re.IGNORECASE)
FAILED_COUNT = re.compile(r"^Number of files failed:\s*([0-9]+)$", re.IGNORECASE)
TIMESTAMP_LINE = re.compile(r"^(?:The )?signature is timestamped:\s*\S", re.IGNORECASE)
TIMESTAMP_CHAIN = re.compile(r"^Timestamp Verified by:\s*$", re.IGNORECASE)
RFC3161 = re.compile(r"\bRFC\s*3161\b", re.IGNORECASE)
NEGATIVE_LINE = re.compile(
    r"\b(?:error|failed|not signed|unsigned|not timestamped|not verified|"
    r"invalid signature|signature verification failed|could not verify)\b",
    re.IGNORECASE)


class SigningError(Exception):
    """Fail-closed error whose text contains no credential value."""


def resolve(value, arg_name, env_name, env):
    """Resolve one input and report whether it came from an arg or the env."""
    if value is None or value == "":
        value = env.get(env_name)
        source = "env:" + env_name
    else:
        source = "arg:" + arg_name
    if not isinstance(value, str) or not value.strip():
        raise SigningError(f"{env_name} is missing or empty")
    if re.search(r"[\x00-\x1f\x7f]", value):
        raise SigningError(f"{env_name} contains a control character")
    return value, source


def validate_timestamp(value):
    if len(value) > MAX_PATH_CHARS or re.search(r"\s", value):
        raise SigningError(f"{TIMESTAMP_ENV} is not a valid RFC 3161 URL")
    if not value.lower().startswith("https://") or not value[8:].split("/", 1)[0]:
        raise SigningError(f"{TIMESTAMP_ENV} must be an https RFC 3161 URL")
    return value


def build_plan(tool, pfx, password, timestamp, target, provenance):
    """The single source of the exact sign argv; no other caller builds it."""
    if any(re.search(r"[\x00-\x1f\x7f]", value) for value in (pfx, password, timestamp, target)):
        raise SigningError("signing input contains a control character")
    return {
        "schema": SCHEMA,
        "operation": "sign",
        "tool": tool,
        "argv": [tool, "sign", "/fd", "sha256", "/tr", timestamp, "/td", "sha256",
                 "/f", pfx, "/p", password, target],
        "provenance": provenance,
    }


def plan_inputs(args, env):
    pfx, pfx_from = resolve(args.pfx, "--pfx", PFX_ENV, env)
    password, password_from = resolve(args.password, "--password", PASSWORD_ENV, env)
    timestamp, timestamp_from = resolve(args.timestamp, "--timestamp", TIMESTAMP_ENV, env)
    target, target_from = resolve(args.file, "file", FILE_ENV, env)
    validate_timestamp(timestamp)
    return pfx, password, timestamp, target, {
        "pfx": pfx_from, "password": password_from,
        "timestamp": timestamp_from, "file": target_from,
    }


def locate_signtool(value, env):
    if value:
        tool, _ = resolve(value, "--signtool", SIGNTOOL_ENV, env)
        return tool
    found = shutil.which("signtool")
    if found:
        return found
    kits = env.get("ProgramFiles(x86)") or env.get("PROGRAMFILES")
    if kits:
        pattern = str(Path(kits) / "Windows Kits" / "10" / "bin" / "*" / "x64" / "signtool.exe")
        candidates = sorted((path for path in glob.glob(pattern) if Path(path).is_file()),
                            reverse=True)
        if candidates:
            return candidates[0]
    raise SigningError("signtool was not found on PATH or in the Windows SDK")


def secret_variants(*values):
    variants = set()
    for value in values:
        if not value:
            continue
        variants.add(value)
        variants.add(value.replace("/", "\\"))
        variants.add(value.replace("\\", "/"))
    return sorted(variants, key=len, reverse=True)


def scrub(text, secrets):
    """Bound and remove every secret variant before text becomes evidence."""
    if isinstance(text, bytes):
        text = text.decode("utf-8", "replace")
    if not isinstance(text, str):
        return ""
    result = text
    for value in secret_variants(*secrets):
        result = result.replace(value, "<redacted>")
    return result[:MAX_TEXT_CHARS]


def as_text(value):
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    return value if isinstance(value, str) else ""


def subprocess_runner(argv):
    return subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=TOOL_TIMEOUT_SECONDS, check=False)


def run_tool(argv, runner, secrets=()):
    """Return (exit_code, scrubbed_bounded_text, truncated); never leak argv."""
    runner = subprocess_runner if runner is None else runner
    try:
        result = runner(argv)
    except Exception as exc:
        # str(exc) from subprocess embeds the secret-bearing argv.
        return 1, type(exc).__name__, True
    code = getattr(result, "returncode", None)
    if not isinstance(code, int):
        return 1, "invalid tool result", True
    combined = as_text(getattr(result, "stdout", "")) + as_text(getattr(result, "stderr", ""))
    raw = combined.encode("utf-8", "replace")
    return code, scrub(raw[:MAX_TOOL_BYTES].decode("utf-8", "replace"), secrets), len(raw) > MAX_TOOL_BYTES


def parse_verify(success, text, truncated=False):
    """True only for an exit-clean, explicitly timestamped valid signature."""
    if not success or truncated or len(text) > MAX_TEXT_CHARS:
        return False
    lines = [line.strip() for line in text.splitlines()]
    if any(NEGATIVE_LINE.search(line) for line in lines if not FAILED_COUNT.match(line)):
        return False
    if not any(SUCCESS_LINE.match(line) for line in lines):
        return False
    ok_count = [SUCCESS_COUNT.match(line) for line in lines]
    bad_count = [FAILED_COUNT.match(line) for line in lines]
    if not any(ok_count) or not any(bad_count):
        return False
    if int(next(m.group(1) for m in ok_count if m)) < 1:
        return False
    if any(int(m.group(1)) != 0 for m in bad_count if m):
        return False
    timestamped = (any(TIMESTAMP_LINE.match(line) for line in lines)
                   and (bool(RFC3161.search(text)) or any(TIMESTAMP_CHAIN.match(line) for line in lines)))
    return timestamped


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_evidence(path, record):
    output = Path(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", encoding="utf-8") as stream:
        json.dump(record, stream, sort_keys=True, indent=2, ensure_ascii=True)
        stream.write("\n")


def private_pfx(source):
    """Write a bounded private copy; the caller deletes it on every exit."""
    raw = Path(source).read_bytes()
    if not raw or len(raw) > MAX_PFX_BYTES:
        raise SigningError("signing PFX is empty or exceeds the size bound")
    fd, path = tempfile.mkstemp(prefix="iris-windows-sign-", suffix=".pfx")
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as stream:
            fd = -1
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        if stat.S_IMODE(os.stat(path).st_mode) != 0o600:
            raise SigningError("temporary signing PFX is not mode 0600")
        return path
    except Exception:
        if fd >= 0:
            os.close(fd)
        try:
            os.unlink(path)
        except OSError:
            pass
        raise


def restrict_acl(path, runner, platform, secrets):
    """Grant only the current user read access to the private PFX on Windows."""
    if platform != "nt":
        return  # POSIX mode 0600 was checked when the copy was written.
    user = os.environ.get("USERNAME") or getpass.getuser()
    if not user:
        raise SigningError("cannot identify the current user for the PFX ACL")
    domain = os.environ.get("USERDOMAIN")
    trustee = f"{domain}\\{user}" if domain else user
    code, _, truncated = run_tool(
        ["icacls", path, "/inheritance:r", "/grant:r", f"{trustee}:(R)"], runner, secrets)
    if code != 0 or truncated:
        raise SigningError("temporary signing PFX ACL restriction failed")


def do_sign(args, env, runner, platform):
    pfx, password, timestamp, target, provenance = plan_inputs(args, env)
    tool = locate_signtool(args.signtool, env)
    if len(target) > MAX_PATH_CHARS or not Path(target).is_file():
        raise SigningError("signing target is not a file")
    secrets = (password, pfx)
    pfx_copy = None
    try:
        pfx_copy = private_pfx(pfx)
        restrict_acl(pfx_copy, runner, platform, secrets)
        record = build_plan(tool, pfx_copy, password, timestamp, target, provenance)
        sign_code, sign_text, sign_truncated = run_tool(record["argv"], runner, secrets)
        if sign_code != 0 or sign_truncated:
            raise SigningError("signtool sign failed (exit status or bounded output)")
        verify_argv = [tool, "verify", "/pa", target]
        verify_code, verify_text, verify_truncated = run_tool(verify_argv, runner, secrets)
        if not parse_verify(verify_code == 0, verify_text, verify_truncated):
            raise SigningError("Authenticode signature or RFC 3161 timestamp not verified")
        evidence = dict(
            schema=SCHEMA, artifact=Path(target).name, bytes=Path(target).stat().st_size,
            sha256=sha256_file(target), verified=True, provenance=provenance,
            sign_text=sign_text, verify_text=verify_text,
            note="signed installer verified with an RFC 3161 timestamp",
        )
        write_evidence(args.output, evidence)
        return 0
    finally:
        if pfx_copy is not None:
            try:
                os.unlink(pfx_copy)
            except FileNotFoundError:
                pass
            except OSError:
                raise SigningError("temporary signing PFX could not be removed") from None


def do_verify(args, env, runner):
    target, target_from = resolve(args.file, "file", FILE_ENV, env)
    tool = locate_signtool(args.signtool, env)
    if len(target) > MAX_PATH_CHARS or not Path(target).is_file():
        raise SigningError("verification target is not a file")
    code, text, truncated = run_tool([tool, "verify", "/pa", target], runner)
    verified = parse_verify(code == 0, text, truncated)
    evidence = dict(
        schema=SCHEMA, artifact=Path(target).name, bytes=Path(target).stat().st_size,
        sha256=sha256_file(target), verified=verified,
        provenance={"file": target_from},
        verify_text=text,
        note=("Authenticode signature and RFC 3161 timestamp verified"
              if verified else "Authenticode trust not verified"),
    )
    write_evidence(args.output, evidence)
    if not verified:
        raise SigningError("Authenticode signature or RFC 3161 timestamp not verified")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name, help_text in (("plan", "emit the secret-bearing signtool sign argv"),
                            ("verify", "verify an installer and write scrubbed evidence"),
                            ("sign", "sign, verify and write scrubbed evidence")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("--file", help=f"installer path (or {FILE_ENV})")
        if name != "verify":
            command.add_argument("--pfx", help=f"PFX path (or {PFX_ENV})")
            command.add_argument("--password", help=f"PFX password (or {PASSWORD_ENV})")
            command.add_argument("--timestamp", help=f"RFC 3161 URL (or {TIMESTAMP_ENV})")
        command.add_argument("--signtool", help=f"signtool path (or {SIGNTOOL_ENV})")
        if name != "plan":
            command.add_argument("--output", required=True, help="scrubbed evidence path")
    args = parser.parse_args(argv)
    env = os.environ
    try:
        if args.command == "plan":
            pfx, password, timestamp, target, provenance = plan_inputs(args, env)
            tool = args.signtool or env.get(SIGNTOOL_ENV) or "signtool"
            print(json.dumps(build_plan(tool, pfx, password, timestamp, target, provenance),
                             sort_keys=True))
            return 0
        if args.command == "sign":
            return do_sign(args, env, None, os.name)
        return do_verify(args, env, None)
    except SigningError as error:
        print("Windows signing BLOCKED: " + scrub(str(error)), file=sys.stderr)
        return 1
    except Exception as error:
        print("Windows signing BLOCKED (" + type(error).__name__ + ")", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
