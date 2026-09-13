"""Recompute Phase 0 whole-line savings from reviewed labels; no filter rules."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CAPTURES = ROOT.parent / 'captures' / 'fusion'
MEMBERS = {'run': ['run'], 'test': ['test', 'test-failure'], 'build': ['build']}


def join_streams(stdout, stderr):
    return stdout + (b'\n' if stdout and stderr and not stdout.endswith(b'\n') else b'') + stderr


def tokens(data):
    return (len(data) + 3) // 4


def measure(raw, kept):
    before, after = tokens(raw), tokens(kept)
    return dict(raw_bytes=len(raw), kept_bytes=len(kept), raw_tokens=before,
                kept_tokens=after, token_savings_pct=100 * (before-after)/before if before else None,
                byte_savings_pct=100 * (len(raw)-len(kept))/len(raw) if raw else None)


def main():
    labels = json.loads((ROOT / 'labels-reviewed.json').read_text())
    expected = {f'{name}.{stream}.txt' for names in MEMBERS.values() for name in names
                for stream in ('stdout', 'stderr')}
    entries = labels['files']
    assert len(entries) == len(expected)
    assert {entry['path'] for entry in entries} == expected
    pairs = {}
    for entry in entries:
        raw = (CAPTURES / entry['path']).read_bytes()
        assert hashlib.sha256(raw).hexdigest() == entry['sha256'], entry['path']
        lines = raw.splitlines(keepends=True)
        records = entry['lines']
        assert [r['line'] for r in records] == list(range(1, len(lines)+1)), entry['path']
        assert all(r['label'] in ('keep', 'noise') and r['reason'] for r in records)
        kept = b''.join(line for line, record in zip(lines, records) if record['label'] == 'keep')
        pairs[entry['path']] = (raw, kept)
    samples = {}
    for names in MEMBERS.values():
        for name in names:
            out, err = pairs[f'{name}.stdout.txt'], pairs[f'{name}.stderr.txt']
            raw, kept = join_streams(out[0], err[0]), join_streams(out[1], err[1])
            samples[name] = measure(raw, kept)
            (ROOT / f'{name}.ideal.txt').write_bytes(kept)
    cohorts = {}
    for command, names in MEMBERS.items():
        sums = {key: sum(samples[name][key] for name in names)
                for key in ('raw_bytes', 'kept_bytes', 'raw_tokens', 'kept_tokens')}
        sums['members'] = names
        sums['token_savings_pct'] = 100 * (1-sums['kept_tokens']/sums['raw_tokens'])
        sums['byte_savings_pct'] = 100 * (1-sums['kept_bytes']/sums['raw_bytes'])
        cohorts[command] = sums
    result = dict(method='ceil(UTF-8 bytes/4); byte-preserving line deletion; no ANSI normalization',
                  scope='Initial toy-project sample, not representative production cohorts',
                  held_out='test-restored; not read or measured', samples=samples, cohorts=cohorts)
    (ROOT / 'measurements.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
