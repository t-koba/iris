#!/usr/bin/env python3
"""Prepare digest-pinned preview fixtures in a new, caller-owned directory."""

import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import socket
import sys
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit


USER_AGENT = ('Iris-preview-corpus-smoke/1.0 (https://github.com/t-koba/iris) '
              'Python-urllib/3')


# Load the sibling CLI script explicitly because its filename contains hyphens.
generator_spec = importlib.util.spec_from_file_location(
    'preview_fixture_generator', Path(__file__).with_name('generate-preview-fixture.py'))
generator = importlib.util.module_from_spec(generator_spec)
generator_spec.loader.exec_module(generator)
GENERATORS = {
    'gradient-jpeg-v1': generator.generate_gradient_jpeg,
    'gradient-jpeg-v1-200': generator.generate_small_gradient_jpeg,
}


def validate_url(url, hosts):
    parts = urlsplit(url)
    if (parts.scheme != "https" or parts.hostname not in hosts
            or parts.username is not None or parts.password is not None
            or parts.port not in (None, 443) or parts.fragment
            or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in url)):
        raise ValueError("fixture URL must use an allowed HTTPS host without credentials")
    return url


def notes_have_source_url(notes):
    """True when free-text notes contain a usable HTTPS URL with a hostname.

    Offline inspection only: no request is made and download-host rules are
    unchanged. A bare "https://" scheme has no hostname and does not count.
    """
    for token in notes.split():
        parts = urlsplit(token)
        if parts.scheme == "https" and parts.hostname:
            return True
    return False


class SafeRedirects(urllib.request.HTTPRedirectHandler):
    def __init__(self, hosts):
        super().__init__()
        self.hosts = hosts

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        validate_url(newurl, self.hosts)  # Before urllib sends the next request.
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def validate_manifest(manifest, budget, only=None):
    hosts = manifest['allowed_hosts']
    if not hosts or any(not isinstance(h, str) or not re.fullmatch(r"[a-z0-9]+(?:[.-][a-z0-9]+)*", h)
                        for h in hosts):
        raise ValueError("allowed_hosts must contain exact lowercase DNS names")
    files = manifest['files']
    if not files:
        raise ValueError("empty corpus")
    names = set()
    total = 0
    for entry in files:
        name = entry['filename']
        # Single portable basename, never a path, hidden name or Windows device.
        if (not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name)
                or name.endswith('.') or name.split('.')[0].upper() in
                {'CON', 'PRN', 'AUX', 'NUL', *(f'COM{i}' for i in range(1, 10)),
                 *(f'LPT{i}' for i in range(1, 10))} or name.casefold() in names):
            raise ValueError("unsafe or duplicate fixture filename")
        names.add(name.casefold())
        if type(entry['size']) is not int or entry['size'] <= 0:
            raise ValueError("fixture size must be a positive integer")
        if not re.fullmatch(r"[0-9a-f]{64}", entry['sha256']):
            raise ValueError("fixture SHA-256 must be lowercase hex")
        if ('url' in entry) == ('generator' in entry):
            raise ValueError("fixture must specify exactly one of url or generator")
        if 'url' in entry:
            validate_url(entry['url'], hosts)
            notes = entry.get('notes')
            if (not isinstance(notes, str) or not notes.strip()
                    or not notes_have_source_url(notes)):
                raise ValueError("external fixture notes must include a source/license URL")
        elif not isinstance(entry['generator'], str) or entry['generator'] not in GENERATORS:
            raise ValueError("unknown fixture generator")
        total += entry['size']
    if only is not None:
        # Every entry in the pinned manifest was validated above; the
        # caller's budget bounds just the one fixture it asked to fetch.
        selected = [entry for entry in files if entry['filename'] == only]
        if not selected:
            raise ValueError(f"unknown preview fixture: {only}")
        if budget <= 0 or selected[0]['size'] > budget:
            raise ValueError("declared corpus exceeds IRIS_PREVIEW_CORPUS_MAX_BYTES")
        return hosts, selected
    if budget <= 0 or total > budget:
        raise ValueError("declared corpus exceeds IRIS_PREVIEW_CORPUS_MAX_BYTES")
    return hosts, files


def verify_pins(entry, size, digest):
    if size != entry['size'] or digest != entry['sha256']:
        raise ValueError("fixture size or SHA-256 mismatch")


def generate_entry(entry, output):
    data = GENERATORS[entry['generator']]()
    size = len(data)
    verify_pins(entry, size, hashlib.sha256(data).hexdigest())
    partial = output / f".{entry['filename']}.part"
    with partial.open('xb') as dest:
        dest.write(data)
    os.replace(partial, output / entry['filename'])
    return size


def transient_fetch_error(error):
    if isinstance(error, urllib.error.HTTPError):
        return error.code in {408, 429, 500, 502, 503, 504}
    reason = error.reason if isinstance(error, urllib.error.URLError) else error
    if isinstance(reason, socket.gaierror):
        return reason.errno == socket.EAI_AGAIN
    return isinstance(reason, (TimeoutError, ConnectionError))


def fetch_entry(opener, entry, output, hosts, timeout, retries, retry_delay, max_retry_delay):
    backoff = min(retry_delay, max_retry_delay)
    destination = output / entry['filename']
    for attempt in range(retries + 1):
        partial = output / f".{entry['filename']}.{attempt}.part"
        request = urllib.request.Request(entry['url'], headers={
            'User-Agent': USER_AGENT, 'Accept-Encoding': 'identity'})
        try:
            with opener.open(request, timeout=timeout) as response:
                validate_url(response.geturl(), hosts)
                if response.status != 200:
                    raise ValueError("fixture response must be HTTP 200")
                length = response.headers.get('Content-Length')
                if length is not None and int(length) != entry['size']:
                    raise ValueError("fixture Content-Length differs from pinned size")
                digest = hashlib.sha256()
                size = 0
                with partial.open('xb') as dest:
                    while True:
                        # One unretained probe byte detects an oversized response.
                        chunk = response.read(min(64 * 1024, entry['size'] - size + 1))
                        if not chunk:
                            break
                        size += len(chunk)
                        if size > entry['size']:
                            raise ValueError("fixture exceeds pinned size")
                        dest.write(chunk)
                        digest.update(chunk)
                verify_pins(entry, size, digest.hexdigest())
            os.replace(partial, destination)
            return size
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ConnectionError) as error:
            partial.unlink(missing_ok=True)
            if isinstance(error, urllib.error.HTTPError):
                error.close()
            if not transient_fetch_error(error) or attempt == retries:
                raise
            print(f"transient preview fixture fetch failure; retry {attempt + 1} of {retries}", file=sys.stderr)
            delay = backoff
            if isinstance(error, urllib.error.HTTPError) and error.code in {429, 503}:
                retry_after = (error.headers or {}).get('Retry-After', '').strip()
                # Only numeric seconds, not HTTP dates; the operator's cap always wins.
                if re.fullmatch(r"[0-9]+(?:\.[0-9]+)?", retry_after):
                    delay = max(delay, float(retry_after))
            delay = min(delay, max_retry_delay)
            if delay:
                time.sleep(delay)
            backoff = min(backoff * 2, max_retry_delay)


def fetch(manifest, output, budget, timeout, opener=None, retries=0, retry_delay=0,
          max_retry_delay=30, only=None):
    hosts, files = validate_manifest(manifest, budget, only)
    if type(retries) is not int or retries < 0:
        raise ValueError("retries must be a non-negative integer")
    for delay in (retry_delay, max_retry_delay):
        if (not isinstance(delay, (int, float)) or isinstance(delay, bool)
                or not math.isfinite(delay) or delay < 0):
            raise ValueError("retry delays must be finite and non-negative")
    opener = opener or urllib.request.build_opener(SafeRedirects(hosts))
    # Refuse reuse, including symlinks: never truncate an existing caller file.
    output.mkdir(mode=0o700)
    try:
        for entry in files:
            if 'generator' in entry:
                size = generate_entry(entry, output)
            else:
                size = fetch_entry(opener, entry, output, hosts, timeout, retries, retry_delay,
                                   max_retry_delay)
            print(f"verified {entry['filename']}: {size} bytes")
    except BaseException:
        shutil.rmtree(output)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=Path('tests/preview-corpus/fixtures.json'))
    parser.add_argument('--output', type=Path, required=True, help='new directory; must not already exist')
    parser.add_argument('--only', help='fetch only this manifest filename; full manifest is still validated')
    args = parser.parse_args()
    # No implicit download allowance. CI/operator must choose the total budget.
    budget = int(os.environ['IRIS_PREVIEW_CORPUS_MAX_BYTES'])
    timeout = float(os.environ.get('IRIS_PREVIEW_CORPUS_TIMEOUT_SECONDS', '30'))
    retries = int(os.environ.get('IRIS_PREVIEW_CORPUS_RETRIES', '0'))
    retry_delay = float(os.environ.get('IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS', '2'))
    max_retry_delay = float(os.environ.get('IRIS_PREVIEW_CORPUS_MAX_RETRY_DELAY_SECONDS', '30'))
    if not 0 < timeout < float('inf'):
        raise ValueError("timeout must be finite and positive")
    fetch(json.loads(args.manifest.read_text(encoding='utf-8')), args.output, budget, timeout,
          retries=retries, retry_delay=retry_delay, max_retry_delay=max_retry_delay,
          only=args.only)


if __name__ == '__main__':
    main()
