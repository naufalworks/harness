"""Executable reference spec for P7-T05 safe incremental publication.

safety::redact is line granular: a suspicious line is dropped whole and a private-key
block spans lines, so the only unit that can be published before [DONE] is a completed
line. This file derives the marker vocabulary, the token thresholds and the redaction
marker from src/safety.rs so the reference cannot drift from Rust, then proves the
invariants in docs/design/incremental-publication.md.

This is a specification and test corpus, NOT a substitute for the Rust implementation,
for cargo, or for the HTTP suites. All fixtures are synthetic: no provider calls, no
real credentials and no user data.
"""
import random, re, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SAFETY = (ROOT / 'src/safety.rs').read_text()


def _rules():
    """Read the sensitivity rules out of src/safety.rs instead of restating them."""
    body = SAFETY[SAFETY.index('pub fn sensitive'):SAFETY.index('pub fn redact')]
    array = body[body.index('['):body.index('.iter()')]
    markers = re.findall(r'"([^"\\]+)"', array)
    sk = re.search(r'starts_with\("sk-"\)\s*&&\s*w\.len\(\)\s*>\s*(\d+)', body)
    akia = re.search(r'starts_with\("AKIA"\)\s*&&\s*w\.len\(\)\s*==\s*(\d+)', body)
    marker = re.search(r'"(\[REDACTED[^"]*\])"', SAFETY)
    assert markers and sk and akia and marker, 'safety.rs shape changed; update this spec'
    return markers, int(sk.group(1)), int(akia.group(1)), marker.group(1)


MARKERS, SK_MIN_LEN, AKIA_LEN, MARKER = _rules()


def sensitive(line):
    """Mirror of safety::sensitive. Words hold only ASCII alphanumerics, '_' and '-',
    so Rust's byte length and Python's character length agree."""
    lower = line.lower()
    if any(m in lower for m in MARKERS):
        return True
    for word in re.split(r'[^0-9A-Za-z_-]', line):
        if word.startswith('sk-') and len(word) > SK_MIN_LEN:
            return True
        if word.startswith('AKIA') and len(word) == AKIA_LEN:
            return True
    return False


def classify(line, in_key):
    """The branch ladder of safety::redact for one line -> (emit_or_None, in_key)."""
    upper = line.upper()
    if '-----BEGIN' in upper and 'PRIVATE KEY-----' in upper:
        return MARKER, '-----END' not in upper
    if in_key:
        return None, '-----END' not in upper
    if sensitive(line):
        return MARKER, False
    return line, False


def redact_whole(text):
    """Reference for today's whole-answer safety::redact."""
    kept, in_key = [], False
    for line in text.split('\n'):
        emit, in_key = classify(line, in_key)
        if emit is not None:
            kept.append(emit)
    return '\n'.join(kept)


class StreamRedactor:
    """Reference for safety::StreamRedactor: publish completed lines only."""

    def __init__(self):
        self.pending = ''
        self.in_key = False
        self.emitted = False
        self.published = ''

    def _emit(self, value):
        if value is None:
            return ''
        text = value if not self.emitted else '\n' + value
        self.emitted = True
        self.published += text
        return text

    def push(self, text):
        self.pending += text
        out = ''
        while '\n' in self.pending:
            line, self.pending = self.pending.split('\n', 1)
            emit, self.in_key = classify(line, self.in_key)
            out += self._emit(emit)
        return out

    def finish(self):
        emit, self.in_key = classify(self.pending, self.in_key)
        self.pending = ''
        return self._emit(emit)

    def abandon(self):
        """Failure or interruption: the withheld tail is dropped, never published."""
        self.pending = ''
        return ''


SK_TOKEN = 'sk-' + 'x' * 20
AKIA_TOKEN = 'AKIA' + 'Q' * 16
GH_TOKEN = 'ghp_' + 'y' * 20
KEY_BODY = 'synthetic-payload'
KEY_BLOCK = ('before\n-----BEGIN PRIVATE KEY-----\n' + KEY_BODY
             + '\n-----END PRIVATE KEY-----\nafter')

FIXTURES = [
    ('plain answer', 'Added retry with backoff.\nTwo files changed.', []),
    ('secret line between safe lines', 'Here it is.\npassword=hidden\nAll done.', ['hidden']),
    ('secret split with no trailing newline', 'pass' + 'word=hidden', ['hidden']),
    ('private key block', KEY_BLOCK, [KEY_BODY]),
    ('unicode stays intact', 'Halo \U0001f980\ncaf\u00e9 \U0001f600\nok', []),
    ('token shapes', 'token: ' + SK_TOKEN + '\naws ' + AKIA_TOKEN + ' here\ntail',
     [SK_TOKEN, AKIA_TOKEN]),
    ('carriage returns survive', 'line one\r\nkey: ' + GH_TOKEN + '\r\ndone', [GH_TOKEN]),
    ('trailing newline', 'answer\n', []),
    ('empty answer', '', []),
]


def splittings(text):
    """Every single cut, one character per chunk, and seeded multi-cut chunkings."""
    yield [text]
    for cut in range(1, len(text)):
        yield [text[:cut], text[cut:]]
    yield list(text) or ['']
    rng = random.Random(20260911)
    for _ in range(20):
        room = max(0, len(text) - 1)
        cuts = sorted(rng.sample(range(1, len(text)), min(3, room))) if room else []
        parts, last = [], 0
        for cut in cuts:
            parts.append(text[last:cut])
            last = cut
        parts.append(text[last:])
        yield parts


class IncrementalPublicationSpec(unittest.TestCase):
    def test_reference_mirrors_the_rust_redaction_unit_tests(self):
        self.assertEqual(redact_whole('Halo \U0001f980\nhello'), 'Halo \U0001f980\nhello')
        self.assertNotIn('synthetic', redact_whole('api_key=synthetic'))
        out = redact_whole(KEY_BLOCK)
        self.assertNotIn(KEY_BODY, out)
        self.assertTrue(out.endswith('after'))
        self.assertIn('private key', MARKERS)
        self.assertTrue(SK_MIN_LEN > 0 and AKIA_LEN > 0)

    def test_streaming_publication_equals_whole_answer_redaction(self):
        for name, text, _ in FIXTURES:
            expected = redact_whole(text)
            for parts in splittings(text):
                r = StreamRedactor()
                out = ''.join(r.push(part) for part in parts) + r.finish()
                self.assertEqual(out, expected, '%s: %r' % (name, parts))
                self.assertEqual(r.published, expected, name)

    def test_published_text_is_always_a_prefix_and_never_leaks_early(self):
        for name, text, forbidden in FIXTURES:
            expected = redact_whole(text)
            for parts in splittings(text):
                r = StreamRedactor()
                for part in parts:
                    r.push(part)
                    self.assertEqual(r.published, expected[:len(r.published)], name)
                    for secret in forbidden:
                        self.assertNotIn(secret, r.published, name)
                r.finish()
                for secret in forbidden:
                    self.assertNotIn(secret, r.published, name)

    def test_partial_line_is_withheld_until_its_terminator(self):
        r = StreamRedactor()
        self.assertEqual(r.push('pass'), '')
        self.assertEqual(r.push('word=hidden'), '')
        self.assertEqual(r.push('\n'), MARKER)
        self.assertEqual(r.push('safe tail'), '')
        self.assertEqual(r.finish(), '\nsafe tail')

    def test_failed_or_interrupted_stream_discards_the_withheld_tail(self):
        r = StreamRedactor()
        r.push('visible line\npass')
        self.assertEqual(r.published, 'visible line')
        self.assertEqual(r.abandon(), '')
        self.assertEqual(r.published, 'visible line')
        self.assertEqual(r.pending, '')

    def test_single_line_answer_is_never_worse_than_whole_answer_buffering(self):
        r = StreamRedactor()
        self.assertEqual(r.push('one long line with no newline at all'), '')
        self.assertEqual(r.finish(), 'one long line with no newline at all')

    def test_private_key_block_publishes_one_marker_and_no_body(self):
        r = StreamRedactor()
        self.assertEqual(r.push('before\n'), 'before')
        self.assertEqual(r.push('-----BEGIN PRIVATE KEY-----\n'), '\n' + MARKER)
        self.assertEqual(r.push(KEY_BODY + '\n'), '')
        self.assertEqual(r.push('-----END PRIVATE KEY-----\n'), '')
        self.assertEqual(r.push('after'), '')
        self.assertEqual(r.finish(), '\nafter')
        self.assertEqual(r.published, redact_whole(KEY_BLOCK))


if __name__ == '__main__':
    unittest.main(verbosity=2)
