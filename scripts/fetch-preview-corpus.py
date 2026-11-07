#!/usr/bin/env python3
"""Fetch digest-pinned preview fixtures into a new, caller-owned directory."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import urllib.request
from urllib.parse import urlsplit


def validate_url(url, hosts):
    parts = urlsplit(url)
    if (parts.scheme != "https" or parts.hostname not in hosts
            or parts.username is not None or parts.password is not None
            or parts.port not in (None, 443) or parts.fragment
            or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in url)):
        raise ValueError("fixture URL must use an allowed HTTPS host without credentials")
    return url


class SafeRedirects(urllib.request.HTTPRedirectHandler):
    def __init__(self, hosts):
        super().__init__()
        self.hosts = hosts

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        validate_url(newurl, self.hosts)  # Before urllib sends the next request.
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def validate_manifest(manifest, budget):
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
        validate_url(entry['url'], hosts)
        total += entry['size']
    if budget <= 0 or total > budget:
        raise ValueError("declared corpus exceeds IRIS_PREVIEW_CORPUS_MAX_BYTES")
    return hosts, files


def fetch(manifest, output, budget, timeout, opener=None):
    hosts, files = validate_manifest(manifest, budget)
    opener = opener or urllib.request.build_opener(SafeRedirects(hosts))
    # Refuse reuse, including symlinks: never truncate an existing caller file.
    output.mkdir(mode=0o700)
    try:
        for entry in files:
            request = urllib.request.Request(entry['url'], headers={
                'User-Agent': 'Iris-preview-corpus-smoke/1.0', 'Accept-Encoding': 'identity'})
            with opener.open(request, timeout=timeout) as response:
                validate_url(response.geturl(), hosts)
                if response.status != 200:
                    raise ValueError("fixture response must be HTTP 200")
                length = response.headers.get('Content-Length')
                if length is not None and int(length) != entry['size']:
                    raise ValueError("fixture Content-Length differs from pinned size")
                digest = hashlib.sha256()
                size = 0
                with (output / entry['filename']).open('xb') as dest:
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
                if size != entry['size'] or digest.hexdigest() != entry['sha256']:
                    raise ValueError("fixture size or SHA-256 mismatch")
            print(f"verified {entry['filename']}: {size} bytes")
    except BaseException:
        shutil.rmtree(output)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=Path('tests/preview-corpus/fixtures.json'))
    parser.add_argument('--output', type=Path, required=True, help='new directory; must not already exist')
    args = parser.parse_args()
    # No implicit download allowance. CI/operator must choose the total budget.
    budget = int(os.environ['IRIS_PREVIEW_CORPUS_MAX_BYTES'])
    timeout = float(os.environ.get('IRIS_PREVIEW_CORPUS_TIMEOUT_SECONDS', '30'))
    if not 0 < timeout < float('inf'):
        raise ValueError("timeout must be finite and positive")
    fetch(json.loads(args.manifest.read_text(encoding='utf-8')), args.output, budget, timeout)


if __name__ == '__main__':
    main()
