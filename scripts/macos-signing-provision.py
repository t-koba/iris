#!/usr/bin/env python3
"""Provision tag-release macOS signing credentials in an isolated keychain.

Every input comes from the environment. Diagnostics name variables and command
steps only: arguments (which contain credentials) are never printed, and the
temporary .p12 is removed on every exit path.
"""
import base64
import binascii
import os
import re
import stat
import subprocess
import sys
import tempfile

REQUIRED_NAMES = (
    "IRIS_MACOS_SIGN_IDENTITY",
    "IRIS_MACOS_INSTALLER_IDENTITY",
    "IRIS_MACOS_NOTARY_PROFILE",
    "IRIS_MACOS_INSTALLER_NOTARY_PROFILE",
    "IRIS_MACOS_SIGNING_P12_BASE64",
    "IRIS_MACOS_SIGNING_P12_PASSWORD",
    "IRIS_MACOS_KEYCHAIN_PASSWORD",
    "IRIS_MACOS_APPLE_ID",
    "IRIS_MACOS_TEAM_ID",
    "IRIS_MACOS_APP_PASSWORD",
    "IRIS_MACOS_KEYCHAIN_PATH",
)
MAX_BYTES_NAME = "IRIS_MACOS_SIGNING_P12_MAX_BYTES"
DEFAULT_MAX_P12_BYTES = 1024 * 1024
KEYCHAIN_TIMEOUT_SECONDS = "21600"
KEY_PARTITION_LIST = "apple-tool:,apple:"
IDENTITY_POLICIES = (
    ("IRIS_MACOS_SIGN_IDENTITY", "codesigning"),
    ("IRIS_MACOS_INSTALLER_IDENTITY", "basic"),
)


class ProvisioningError(Exception):
    """Fail-closed error whose text contains names, never credential values."""


def require(env, name):
    value = env.get(name)
    if not isinstance(value, str) or not value.strip():
        raise ProvisioningError(f"{name} is missing or empty")
    return value


def load_config(env):
    config = {name: require(env, name) for name in REQUIRED_NAMES}
    for name in ("IRIS_MACOS_SIGN_IDENTITY", "IRIS_MACOS_INSTALLER_IDENTITY"):
        if config[name] == "-":
            raise ProvisioningError(f"{name} must not be the ad-hoc selector '-'")
    raw_max = env.get(MAX_BYTES_NAME)
    if raw_max is None or raw_max == "":
        max_bytes = DEFAULT_MAX_P12_BYTES
    elif re.fullmatch(r"[0-9]+", raw_max) and int(raw_max) > 0:
        max_bytes = int(raw_max)
    else:
        raise ProvisioningError(f"{MAX_BYTES_NAME} must be a positive integer")

    encoded = config["IRIS_MACOS_SIGNING_P12_BASE64"]
    if len(encoded) > 4 * ((max_bytes + 2) // 3):
        raise ProvisioningError(
            "IRIS_MACOS_SIGNING_P12_BASE64 exceeds IRIS_MACOS_SIGNING_P12_MAX_BYTES"
        )
    try:
        p12 = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError):
        raise ProvisioningError("IRIS_MACOS_SIGNING_P12_BASE64 is not valid base64") from None
    if not p12:
        raise ProvisioningError("IRIS_MACOS_SIGNING_P12_BASE64 is empty")
    if len(p12) > max_bytes:
        raise ProvisioningError(
            "IRIS_MACOS_SIGNING_P12_BASE64 exceeds IRIS_MACOS_SIGNING_P12_MAX_BYTES"
        )
    return config, p12


def write_p12(p12):
    fd, path = tempfile.mkstemp(prefix="iris-signing-", suffix=".p12")
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as stream:
            fd = -1
            stream.write(p12)
            stream.flush()
            os.fsync(stream.fileno())
        if stat.S_IMODE(os.stat(path).st_mode) != 0o600:
            raise ProvisioningError("temporary signing .p12 is not mode 0600")
        return path
    except Exception:
        if fd >= 0:
            os.close(fd)
        try:
            os.unlink(path)
        except OSError:
            pass
        raise


def subprocess_runner(argv):
    """Production runner: check every step and capture all tool output."""
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, text=True)


def invoke(argv, label, runner):
    try:
        result = runner(argv)
    except Exception as exc:
        # Never include str(exc): subprocess embeds the secret-bearing argv.
        raise ProvisioningError(f"{label} failed ({type(exc).__name__})") from None
    if getattr(result, "returncode", 0) != 0:
        raise ProvisioningError(f"{label} failed (non-zero exit status)")
    output = getattr(result, "stdout", "")
    return output if isinstance(output, str) else ""


def identity_present(output, selector):
    return selector in re.findall(r'"([^"]+)"', output)


def provision(env=None, runner=None):
    """Create the keychain, import the p12, store both notary profiles, verify."""
    env = os.environ if env is None else env
    runner = subprocess_runner if runner is None else runner
    config, p12 = load_config(env)
    keychain = config["IRIS_MACOS_KEYCHAIN_PATH"]
    keychain_password = config["IRIS_MACOS_KEYCHAIN_PASSWORD"]
    p12_path = None
    try:
        p12_path = write_p12(p12)
        invoke(["security", "create-keychain", "-p", keychain_password, keychain],
               "security create-keychain", runner)
        invoke(["security", "set-keychain-settings", "-lut",
                KEYCHAIN_TIMEOUT_SECONDS, keychain],
               "security set-keychain-settings", runner)
        invoke(["security", "unlock-keychain", "-p", keychain_password, keychain],
               "security unlock-keychain", runner)
        invoke(["security", "import", p12_path, "-k", keychain, "-P",
                config["IRIS_MACOS_SIGNING_P12_PASSWORD"],
                "-T", "/usr/bin/codesign", "-T", "/usr/bin/productsign"],
               "security import", runner)
        invoke(["security", "set-key-partition-list", "-S", KEY_PARTITION_LIST,
                "-s", "-k", keychain_password, keychain],
               "security set-key-partition-list", runner)
        invoke(["security", "list-keychains", "-d", "user", "-s", keychain],
               "security list-keychains", runner)
        for profile in ("IRIS_MACOS_NOTARY_PROFILE",
                        "IRIS_MACOS_INSTALLER_NOTARY_PROFILE"):
            invoke(["xcrun", "notarytool", "store-credentials", config[profile],
                    "--apple-id", config["IRIS_MACOS_APPLE_ID"],
                    "--team-id", config["IRIS_MACOS_TEAM_ID"],
                    "--password", config["IRIS_MACOS_APP_PASSWORD"],
                    "--keychain", keychain],
                   "xcrun notarytool store-credentials", runner)
        for name, policy in IDENTITY_POLICIES:
            output = invoke(["security", "find-identity", "-v", "-p", policy, keychain],
                            "security find-identity", runner)
            if not identity_present(output, config[name]):
                raise ProvisioningError(f"{name} was not found in the provisioned keychain")
    finally:
        if p12_path is not None:
            try:
                os.unlink(p12_path)
            except FileNotFoundError:
                pass
            except OSError:
                raise ProvisioningError("temporary signing .p12 could not be removed") from None


def main(argv=None):
    if argv is None:
        argv = sys.argv[1:]
    if argv:
        print("macOS signing credential provisioning takes no arguments", file=sys.stderr)
        return 2
    try:
        provision()
    except ProvisioningError as exc:
        print(f"macOS signing credential provisioning failed: {exc}", file=sys.stderr)
        return 1
    except Exception as exc:
        print(f"macOS signing credential provisioning failed ({type(exc).__name__})",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
