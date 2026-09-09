#!/usr/bin/env python3
"""Bounded candle/entropy probes using the existing frozen inputs and Rust tools.

No data preparation or expensive level-19 book compression is repeated. Results
are private local artifacts. This runner owns the host lock and four existing
encoder slots; do not wrap it in flock.
"""
from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def save(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + '\n')


def utc() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus-root', type=Path, required=True, help='Frozen corpus-w03 directory')
    parser.add_argument('--bin-dir', type=Path, required=True, help='Cargo release directory; examples are under examples/')
    parser.add_argument('--output', type=Path, required=True, help='New directory outside the source tree')
    parser.add_argument('--commit', required=True, help='Exact source revision used to build the binaries')
    parser.add_argument('--question', choices=['candles', 'entropy', 'creation', 'all'], default='candles')
    parser.add_argument('--iterations', type=int, default=3)
    parser.add_argument('--warmups', type=int, default=1)
    parser.add_argument('--lock', type=Path, default=Path('/tmp/lean-node-20260907-1000/bench.lock'))
    parser.add_argument('--encoder-slots', type=Path, default=Path('/media/anton/data/grimoire-home/encoder-slots'))
    parser.add_argument('--lock-timeout', type=float, default=60)
    args = parser.parse_args()
    if args.iterations < 1 or args.warmups < 0 or not 0 < args.lock_timeout <= 60:
        parser.error('positive iterations, nonnegative warmups, and lock timeout in (0,60] required')
    args.output.mkdir(parents=True, exist_ok=False)
    fds: list[int] = []
    jobs: list[dict] = []
    frozen: dict[Path, str] = {}

    def acquire(path: Path) -> None:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        fds.append(fd)
        deadline = time.monotonic() + args.lock_timeout
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f'No measurement slot: {path}')
                time.sleep(0.2)

    def remember(path: Path) -> Path:
        if path not in frozen:
            frozen[path] = digest(path)
            save(args.output / "identities.json", {str(p): sha for p, sha in frozen.items()})
        return path

    def binary(name: str, example: bool = False) -> Path:
        return remember(args.bin_dir / ('examples' if example else '') / name)

    def run(name: str, command: list) -> None:
        command = [str(x) for x in command]
        print('START', name, utc(), flush=True)
        start = time.monotonic_ns()
        with (args.output / (name + '.stdout')).open('wb') as out, (args.output / (name + '.stderr')).open('wb') as err:
            proc = subprocess.Popen(command, stdout=out, stderr=err, pass_fds=tuple(fds))
            _, status, usage = os.wait4(proc.pid, 0)
            proc.returncode = os.waitstatus_to_exitcode(status)
        jobs.append(dict(name=name, command=command, returncode=proc.returncode,
                         wall_ns=time.monotonic_ns() - start, cpu_user_s=usage.ru_utime,
                         cpu_system_s=usage.ru_stime, peak_rss_kib=usage.ru_maxrss, finished_utc=utc()))
        save(args.output / 'commands.json', jobs)
        print('DONE', name, proc.returncode, flush=True)
        if proc.returncode:
            raise RuntimeError(f'{name} failed; see stderr')

    def probe(name: str, source: Path, creation: bool = True) -> None:
        command = [binary('transmutation_probe', True), '--input', remember(source),
                   '--output-dir', args.output / name, '--iterations', args.iterations,
                   '--warmups', args.warmups, '--file-io', '--zstd-levels', '3']
        if not creation:
            command.append('--skip-creation')
        run(name, command)

    try:
        acquire(args.lock)
        for slot in range(4):
            acquire(args.encoder_slots / f'{slot}.lock')
        save(args.output / 'conditions.json', dict(started_utc=utc(), commit=args.commit,
             threads=1, lock=str(args.lock), encoder_slots=4,
             loadavg=Path('/proc/loadavg').read_text().strip(),
             services='unchanged; existing codec jobs drained naturally',
             cache='memory and warm file; no cold-disk claim',
             rss='wait4 process peak includes setup, references, warmup and verification'))
        if args.question in ('candles', 'all'):
            candle_root = args.corpus_root / 'candles'
            manifest = json.loads(remember(candle_root / 'manifest.json').read_text())
            for name in ('real-candles-es-16384', 'real-candles-nq-16384', 'real-candles-es-tiny16'):
                entry = next(row for row in manifest['datasets'] if row['dataset'] == name)
                metadata = json.loads(remember(candle_root / (name + '.metadata.json')).read_text())
                facts = json.loads(remember(candle_root / (name + '.sourcefacts.json')).read_text())
                rows = json.loads(remember(candle_root / (name + '.input.json')).read_text())
                canonical = lambda value: json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()
                bound = {k: v for k, v in facts.items() if k not in ('format', 'digests')}
                if (hashlib.sha256(canonical(bound)).hexdigest() != entry['all_facts_sha256']
                        or metadata['digests'] != facts['digests'] or facts['encoded_rows'] != rows
                        or hashlib.sha256(canonical(rows)).hexdigest() != facts['digests']['encoded_rows_sha256']):
                    raise ValueError(f'Frozen candle facts changed: {name}')
                probe(name, candle_root / (name + '.aura0'))
        if args.question in ('entropy', 'all'):
            source = remember(args.corpus_root / 'current/bitget_delta.aura0')
            run('structural-pair', [binary('structural_frontier', True), source, args.output / 'structural'])
            probe('production', source, False)
            for name in ('packed', 'huffman'):
                probe(name, args.output / 'structural' / (name + '.aura0'), False)
        if args.question in ('creation', 'all'):
            probe('bitget-creation', args.corpus_root / 'current/bitget_delta.aura0')
        for path, sha in frozen.items():
            if digest(path) != sha:
                raise RuntimeError(f'Input or executable changed: {path}')
        save(args.output / 'identities.json', {str(path): sha for path, sha in frozen.items()})
    finally:
        for fd in reversed(fds):
            os.close(fd)


if __name__ == '__main__':
    main()
