"""Tests for measure.py helpers: estimator rounding, stream join, savings math."""
import unittest

from measure import join_streams, measure, tokens


class TestTokensUtf8Rounding(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(tokens(b''), 0)

    def test_exact_multiple(self):
        self.assertEqual(tokens(b'a' * 8), 2)

    def test_ceils_up(self):
        self.assertEqual(tokens(b'a' * 9), 3)

    def test_utf8_multibyte_counts_bytes(self):
        self.assertEqual(tokens('é'.encode('utf-8')), 1)          # 2 bytes -> 1
        self.assertEqual(tokens('ééé'.encode('utf-8')), 2)        # 6 bytes -> 2
        self.assertEqual(tokens('éééé'.encode('utf-8')), 2)       # 8 bytes -> 2
        self.assertEqual(tokens('ééééé'.encode('utf-8')), 3)      # 10 bytes -> 3


class TestJoinStreams(unittest.TestCase):
    def test_unterminated_stdout_gets_one_separator(self):
        self.assertEqual(
            join_streams(b'Result: 1', b'Error: detail\n  context\n'),
            b'Result: 1\nError: detail\n  context\n')

    def test_terminated_stdout_gets_no_extra_separator(self):
        self.assertEqual(
            join_streams(b'Result: 1\n', b'Error: detail\n'),
            b'Result: 1\nError: detail\n')

    def test_empty_stderr_left_unchanged(self):
        self.assertEqual(join_streams(b'Result: 1\n', b''), b'Result: 1\n')

    def test_unterminated_stdout_with_empty_stderr_unchanged(self):
        self.assertEqual(join_streams(b'Result: 1', b''), b'Result: 1')

    def test_empty_stdout_left_unchanged(self):
        self.assertEqual(join_streams(b'', b'Error: detail\n'), b'Error: detail\n')


class TestMeasureSavings(unittest.TestCase):
    def test_known_example(self):
        raw = b'line one\n' * 10          # 90 bytes -> 23 tokens
        kept = b'line one\n' * 5           # 45 bytes -> 12 tokens
        result = measure(raw, kept)
        self.assertEqual(result['raw_bytes'], 90)
        self.assertEqual(result['kept_bytes'], 45)
        self.assertEqual(result['raw_tokens'], 23)
        self.assertEqual(result['kept_tokens'], 12)
        self.assertAlmostEqual(result['token_savings_pct'], 100 * 11 / 23)
        self.assertAlmostEqual(result['byte_savings_pct'], 50.0)

    def test_empty_raw_is_na(self):
        result = measure(b'', b'')
        self.assertIsNone(result['token_savings_pct'])
        self.assertIsNone(result['byte_savings_pct'])


if __name__ == '__main__':
    unittest.main()
