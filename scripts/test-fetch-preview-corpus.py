#!/usr/bin/env python3
"""Offline boundary tests: no network or downloaded fixtures."""

import copy
import hashlib
import importlib.util
import io
import json
import os
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


def read_corpus_manifest():
    root = Path(__file__).resolve().parents[1]
    return json.loads((root / 'tests/preview-corpus/fixtures.json').read_text(encoding='utf-8'))


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

    def local_jpeg_manifest(self):
        manifest = read_corpus_manifest()
        entry, = [entry for entry in manifest['files'] if entry['filename'] == 'landscape.jpg']
        manifest['files'] = [entry]
        return manifest, entry

    def test_generated_jpeg_matches_pins_without_network(self):
        manifest, entry = self.local_jpeg_manifest()
        opener = Mock()
        helper.fetch(manifest, self.output, entry['size'], 1, opener)
        opener.open.assert_not_called()
        self.assertEqual(list(self.output.iterdir()), [self.output / 'landscape.jpg'])
        data = (self.output / 'landscape.jpg').read_bytes()
        self.assertEqual(len(data), entry['size'])
        self.assertEqual(hashlib.sha256(data).hexdigest(), entry['sha256'])

    def test_generated_jpeg_pin_mismatch_prevents_installation(self):
        manifest, entry = self.local_jpeg_manifest()
        for changes in ({'size': entry['size'] + 1}, {'sha256': '0' * 64}):
            corrupted = dict(manifest, files=[dict(entry, **changes)])
            opener = Mock()
            with self.subTest(changes=changes), patch.object(helper.os, 'replace') as replace:
                with self.assertRaisesRegex(ValueError, 'fixture size or SHA-256 mismatch'):
                    helper.fetch(corrupted, self.output, 40_000_000, 1, opener)
                replace.assert_not_called()
                opener.open.assert_not_called()
                self.assertFalse(self.output.exists())

    def test_manifest_rejects_invalid_fixture_sources(self):
        entry = dict(self.manifest['files'][0])
        url = entry.pop('url')
        for sources, error in (
            ({'url': url, 'generator': 'gradient-jpeg-v1'}, 'exactly one'),
            ({}, 'exactly one'),
            ({'generator': 'unknown'}, 'unknown fixture generator'),
        ):
            manifest = dict(self.manifest, files=[dict(entry, **sources)])
            with self.subTest(sources=sources), self.assertRaisesRegex(ValueError, error):
                helper.validate_manifest(manifest, len(self.body))

    def test_user_agent_has_public_contact(self):
        opener = self.fetch(self.body)
        request = opener.open.call_args.args[0]
        user_agent = request.headers['User-agent']
        self.assertTrue(user_agent.startswith('Iris-preview-corpus-smoke/1.0 ('))
        self.assertIn('https://github.com/t-koba/iris', user_agent)
        self.assertIn('Python-urllib/', user_agent)

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
            urllib.error.HTTPError(self.manifest['files'][0]['url'], 503, 'unavailable', {}, io.BytesIO()),
            urllib.error.HTTPError(self.manifest['files'][0]['url'], 500, 'server error', {}, io.BytesIO()),
            Response(self.body),
        ]
        with patch.object(helper.time, 'sleep') as sleep:
            helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                         retries=4, retry_delay=3, max_retry_delay=10)
        self.assertEqual(opener.open.call_count, 5)
        self.assertEqual(sleep.call_args_list, [unittest.mock.call(n) for n in (3, 6, 10, 10)])
        self.assertEqual((self.output / 'test.jpg').read_bytes(), self.body)
        self.assertEqual([p.name for p in self.output.iterdir()], ['test.jpg'])

    def test_retry_after_is_honoured_before_next_attempt_but_capped(self):
        for index, (code, header, cap, expected) in enumerate((
            (429, '7', 30, 7), (503, '7', 30, 7),
            (429, '100', 5, 5), (503, '100', 5, 5),
            (429, '9' * 400, 5, 5), (429, '7', 0, 0),
            (429, 'bad', 30, 2), (503, '-7', 30, 2),
            (429, 'Wed, 21 Oct 2015 07:28:00 GMT', 30, 2),
            (500, '7', 30, 2),
        )):
            with self.subTest(code=code, header=header, cap=cap), patch.object(helper.time, 'sleep') as sleep:
                error = urllib.error.HTTPError(self.manifest['files'][0]['url'], code,
                                               'transient', {'Retry-After': header}, io.BytesIO())
                output = Path(self.temp.name) / f'corpus-{index}'

                def attempt(*args, **kwargs):
                    if opener.open.call_count == 1:
                        raise error
                    if expected:
                        sleep.assert_called_once_with(expected)
                    else:
                        sleep.assert_not_called()
                    return Response(self.body)

                opener = Mock()
                opener.open.side_effect = attempt
                helper.fetch(self.manifest, output, len(self.body), 1, opener,
                             retries=1, retry_delay=2, max_retry_delay=cap)
                self.assertEqual(opener.open.call_count, 2)
                self.assertEqual((output / 'test.jpg').read_bytes(), self.body)
                self.assertEqual([p.name for p in output.iterdir()], ['test.jpg'])

    def test_retry_exhaustion_removes_output_without_final_sleep(self):
        opener = Mock()
        opener.open.side_effect = urllib.error.URLError(TimeoutError('transient'))
        with patch.object(helper.time, 'sleep') as sleep, self.assertRaises(urllib.error.URLError):
            helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                         retries=3, retry_delay=2, max_retry_delay=4)
        self.assertEqual(opener.open.call_count, 4)
        self.assertEqual(sleep.call_args_list, [unittest.mock.call(n) for n in (2, 4, 4)])
        self.assertFalse(self.output.exists())

    def test_permanent_http_error_and_integrity_mismatch_are_not_retried(self):
        for result, exception in (
            (urllib.error.HTTPError(self.manifest['files'][0]['url'], 404, 'not found',
                                    {'Retry-After': '7'}, io.BytesIO()), urllib.error.HTTPError),
            (Response(b'x' * len(self.body)), ValueError),
        ):
            with self.subTest(result=result), patch.object(helper.time, 'sleep') as sleep:
                opener = Mock()
                opener.open.side_effect = [result]
                with self.assertRaises(exception):
                    helper.fetch(self.manifest, self.output, len(self.body), 1, opener,
                                 retries=4, retry_delay=2)
                self.assertEqual(opener.open.call_count, 1)
                sleep.assert_not_called()
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

    def test_invalid_max_retry_delay_fails_before_output_creation(self):
        for cap in (-1, float('inf'), float('nan'), True, '30'):
            with self.subTest(cap=cap), self.assertRaises(ValueError):
                helper.fetch(self.manifest, self.output, len(self.body), 1, Mock(),
                             max_retry_delay=cap)
            self.assertFalse(self.output.exists())

    def test_main_reads_max_retry_delay_from_environment(self):
        manifest_path = Path(self.temp.name) / 'manifest.json'
        manifest_path.write_text(json.dumps(self.manifest))
        for index, cap in enumerate(('5', '0', '-1', 'inf', 'nan', 'bad')):
            output = Path(self.temp.name) / f'corpus-{index}'
            opener = Mock()
            opener.open.side_effect = [
                urllib.error.HTTPError(self.manifest['files'][0]['url'], 429, 'rate limited',
                                       {'Retry-After': '100'}, io.BytesIO()),
                Response(self.body),
            ]
            with self.subTest(cap=cap), patch.dict(os.environ, {
                'IRIS_PREVIEW_CORPUS_MAX_BYTES': str(len(self.body)),
                'IRIS_PREVIEW_CORPUS_RETRIES': '1',
                'IRIS_PREVIEW_CORPUS_RETRY_DELAY_SECONDS': '2',
                'IRIS_PREVIEW_CORPUS_MAX_RETRY_DELAY_SECONDS': cap,
            }), patch.object(helper.sys, 'argv', [
                'fetch-preview-corpus.py', '--manifest', str(manifest_path), '--output', str(output),
            ]), patch.object(helper.urllib.request, 'build_opener', return_value=opener), \
                    patch.object(helper.time, 'sleep') as sleep:
                if cap in ('5', '0'):
                    helper.main()
                    if cap == '5':
                        sleep.assert_called_once_with(5)
                    else:
                        sleep.assert_not_called()
                    self.assertEqual((output / 'test.jpg').read_bytes(), self.body)
                else:
                    with self.assertRaises(ValueError):
                        helper.main()
                    opener.open.assert_not_called()
                    self.assertFalse(output.exists())

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
        manifest = read_corpus_manifest()
        _, files = helper.validate_manifest(manifest, 40_000_000)
        self.assertEqual(len(files), 6)

    def test_real_manifest_hevc_contract(self):
        manifest = read_corpus_manifest()
        self.assertIn('cdn.truefilesize.com', manifest['allowed_hosts'])
        hevc, = [entry for entry in manifest['files'] if entry['filename'] == 'hevc.mp4']
        self.assertEqual(hevc['url'], 'https://cdn.truefilesize.com/mp4/sample-h265.mp4')
        self.assertEqual(hevc['size'], 4_948_915)
        self.assertEqual(hevc['sha256'],
                         'f110da4f156dde1c5fcf38895109568660b6464aa9c33d3c042d2470e2a9c364')
        total = sum(entry['size'] for entry in manifest['files'])
        self.assertEqual(total, 38_560_017)
        self.assertLessEqual(total, 40_000_000)


if __name__ == '__main__':
    unittest.main()
