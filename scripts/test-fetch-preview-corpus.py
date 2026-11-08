#!/usr/bin/env python3
"""Offline boundary tests: no network and no corpus bytes."""

import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import socket
import tempfile
import unittest
from unittest.mock import Mock, patch
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location('fetch_corpus', Path(__file__).with_name('fetch-preview-corpus.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class Response(io.BytesIO):
    status = 200
    headers = {}

    def geturl(self):
        return 'https://assets.example/fixture'


class FetchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name) / 'corpus'
        self.body = b'test fixture'
        self.manifest = {'allowed_hosts': ['assets.example'], 'files': [{
            'filename': 'test.jpg', 'url': 'https://assets.example/fixture',
            'size': len(self.body), 'sha256': hashlib.sha256(self.body).hexdigest()}]}

    def fetch(self, body, **changes):
        response = Response(body)
        for key, value in changes.items():
            setattr(response, key, value)
        opener = Mock()
        opener.open.return_value = response
        helper.fetch(self.manifest, self.output, len(self.body), 1, opener)
        return opener

    def test_success_and_streamed_size_bound(self):
        self.fetch(self.body)
        self.assertEqual((self.output / 'test.jpg').read_bytes(), self.body)
        self.assertEqual([p.name for p in self.output.iterdir()], ['test.jpg'])

    def test_transient_gateway_and_stream_errors_retry_without_partial_bytes(self):
        interrupted = Response(self.body)
        original_read = interrupted.read
        reads = 0

        def fail_after_partial(size=-1):
            nonlocal reads
            reads += 1
            if reads == 1:
                return original_read(4)
            if reads == 2:
                raise ConnectionResetError('synthetic disconnect')
            return original_read(size)

        interrupted.read = fail_after_partial
        opener = Mock()
        opener.open.side_effect = [
            urllib.error.HTTPError(self.manifest['files'][0]['url'], 504, 'gateway timeout', {}, io.BytesIO()),
            interrupted,
            Response(self.body),
        ]
        with patch.object(helper.time, 'sleep') as sleep:
            helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                         retries=2, retry_delay=3)
        self.assertEqual(opener.open.call_count, 3)
        self.assertEqual(sleep.call_args_list, [unittest.mock.call(3), unittest.mock.call(3)])
        self.assertEqual((self.output / 'test.jpg').read_bytes(), self.body)
        self.assertEqual([p.name for p in self.output.iterdir()], ['test.jpg'])

    def test_permanent_http_error_and_integrity_mismatch_are_not_retried(self):
        for error in (
            urllib.error.HTTPError(self.manifest['files'][0]['url'], 404, 'not found', {}, io.BytesIO()),
        ):
            with self.subTest(code=error.code):
                opener = Mock()
                opener.open.side_effect = error
                with self.assertRaises(urllib.error.HTTPError):
                    helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                                 retries=2, retry_delay=0)
                self.assertEqual(opener.open.call_count, 1)
                self.assertFalse(self.output.exists())
        opener = Mock()
        opener.open.return_value = Response(b'corrupt body')
        with self.assertRaises(ValueError):
            helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                         retries=2, retry_delay=0)
        self.assertEqual(opener.open.call_count, 1)
        self.assertFalse(self.output.exists())

    def test_temporary_dns_resolution_retries_but_permanent_dns_fails_fast(self):
        opener = Mock()
        opener.open.side_effect = [
            urllib.error.URLError(socket.gaierror(socket.EAI_AGAIN, 'temporary resolver failure')),
            Response(self.body),
        ]
        helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                     retries=1, retry_delay=0)
        self.assertEqual(opener.open.call_count, 2)
        self.assertEqual((self.output / 'test.jpg').read_bytes(), self.body)

        permanent = Path(self.temp.name) / 'permanent-corpus'
        opener = Mock()
        opener.open.side_effect = urllib.error.URLError(
            socket.gaierror(socket.EAI_NONAME, 'permanent resolver failure'))
        with self.assertRaises(urllib.error.URLError):
            helper.fetch(self.manifest, permanent, len(self.body), 1, opener,
                         retries=2, retry_delay=0)
        self.assertEqual(opener.open.call_count, 1)
        self.assertFalse(permanent.exists())

    def test_invalid_retry_settings_fail_before_output_creation(self):
        for retries, delay in ((-1, 0), (1.5, 0), (0, -1), (0, float('inf'))):
            with self.subTest(retries=retries, delay=delay), self.assertRaises(ValueError):
                helper.fetch(self.manifest, self.output, len(self.body), 1, Mock(),
                             retries=retries, retry_delay=delay)
            self.assertFalse(self.output.exists())

    def test_corrupt_responses_remove_owned_output(self):
        for body, changes in [
            (self.body[:-1], {}), (self.body + b'excess', {}),
            (b'x' * len(self.body), {}),
            (self.body, {'headers': {'Content-Length': '1000'}}),
            (self.body, {'status': 206}),
            (self.body, {'geturl': lambda: 'https://evil.example/file'}),
        ]:
            with self.subTest(body=body, changes=changes), self.assertRaises(ValueError):
                self.fetch(body, **changes)
            self.assertFalse(self.output.exists())

    def test_manifest_failures_happen_before_network_or_writes(self):
        for field, value in [('filename', '../escape'), ('filename', '/absolute'),
                             ('filename', 'a/b'), ('filename', 'a\\b'),
                             ('filename', 'NUL.jpg'), ('filename', '.hidden'),
                             ('filename', 'trailing.'), ('sha256', 'bad'), ('size', -1),
                             ('url', 'http://assets.example/file')]:
            manifest = copy.deepcopy(self.manifest)
            manifest['files'][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                helper.fetch(manifest, self.output, len(self.body), 1, Mock())
            self.assertFalse(self.output.exists())
        for budget in (0, len(self.body) - 1):
            with self.assertRaises(ValueError):
                helper.fetch(self.manifest, self.output, budget, 1, Mock())
        self.manifest['files'].append(dict(self.manifest['files'][0], filename='TEST.JPG'))
        with self.assertRaises(ValueError):
            helper.validate_manifest(self.manifest, 100)

    def test_initial_and_redirect_url_policy(self):
        handler = helper.SafeRedirects(['assets.example'])
        request = urllib.request.Request('https://assets.example/start')
        for url in ['http://assets.example/x', 'https://assets.example.evil/x',
                    'https://user@assets.example/x', 'https://assets.example:444/x',
                    'https://assets.example/x#fragment', 'https://assets.example/\nx',
                    'file:///tmp/fixture']:
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    helper.validate_url(url, handler.hosts)
                with self.assertRaises(ValueError):
                    handler.redirect_request(request, None, 302, '', {}, url)
        redirected = handler.redirect_request(request, None, 302, '', {}, 'https://assets.example/end')
        self.assertEqual(redirected.full_url, 'https://assets.example/end')

    def test_existing_directory_and_symlink_are_not_reused(self):
        self.output.mkdir()
        sentinel = self.output / 'test.jpg'
        sentinel.write_bytes(b'caller data')
        with self.assertRaises(FileExistsError):
            self.fetch(self.body)
        self.assertEqual(sentinel.read_bytes(), b'caller data')
        link = Path(self.temp.name) / 'link'
        link.symlink_to(self.output, target_is_directory=True)
        with self.assertRaises(FileExistsError):
            helper.fetch(self.manifest, link, len(self.body), 1, Mock())
        self.assertEqual(sentinel.read_bytes(), b'caller data')

    def test_real_manifest_is_within_ci_budget(self):
        root = Path(__file__).resolve().parents[1]
        manifest = json.loads((root / 'tests/preview-corpus/fixtures.json').read_text())
        _, files = helper.validate_manifest(manifest, 40_000_000)
        self.assertEqual(len(files), 5)


if __name__ == '__main__':
    unittest.main()
