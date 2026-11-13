#!/usr/bin/env python3
"""Offline size-only experiment in a private source snapshot; never flashes.

The probe replaces selected command bodies, preserving parser/help and real
features. Deltas include LTO/inlining effects and are not additive budgets.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target' / 'memory-audit'
TARGET = 'riscv32imafc-unknown-none-elf'

JPEG = ['line.strip_prefix("jpeg-idct-bench ")', 'line.strip_prefix("jpeg-profile ")', 'line.strip_prefix("jpeg-bench ")']
MEMORY = ['line == "l2-probe"', 'line == "memory-bench"', 'line == "memory-test"', "command == b'P'"]
SD_TESTS = ['line == "sd-stress"', "command == b'#'", "command == b'@'", "command == b'm'", "command == b'M'"]
OTHER_LABS = ['line == "irq-test"', 'line.strip_prefix("c6-raw ")', 'line == "arbitration status"', 'line.strip_prefix("arbitration bench ")', 'line == "wav-lab"', 'line == "mixed-stress"']
LABS = JPEG + MEMORY + SD_TESTS + OTHER_LABS
SD_CONSOLE = ["command == b'!'", "command == b'Z'", "command == b'Y'"]
WIFI_WEB = ['line.strip_prefix("wifi-web ")']
VARIANTS = {
    'baseline': [],
    'no_jpeg_benchmarks': JPEG,
    'no_memory_experiments': MEMORY,
    'no_sd_tests': SD_TESTS,
    'no_other_labs': OTHER_LABS,
    'no_all_labs': LABS,
    'no_wifi_web_specialization': WIFI_WEB,
    'no_labs_or_sd_console': LABS + SD_CONSOLE,
    'no_labs_sd_console_or_wifi_web': LABS + SD_CONSOLE + WIFI_WEB,
}

def body_end(source, start):
    """Match Rust braces, ignoring strings, chars and nested comments."""
    depth, i = 0, start
    while i < len(source):
        if source.startswith('//', i):
            j = source.find('\n', i); i = len(source) if j < 0 else j + 1; continue
        if source.startswith('/*', i):
            nesting, i = 1, i + 2
            while nesting:
                if source.startswith('/*', i): nesting += 1; i += 2
                elif source.startswith('*/', i): nesting -= 1; i += 2
                else: i += 1
                if i >= len(source): raise ValueError('unclosed comment')
            continue
        if source[i] == '"':
            i += 1
            while i < len(source) and source[i] != '"':
                i += 2 if source[i] == '\\' else 1
            i += 1; continue
        if source[i] == "'":
            char = re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|.)|[^'\\\n])'", source[i:])
            if char: i += len(char[0]); continue
        if source[i] == '{': depth += 1
        elif source[i] == '}':
            depth -= 1
            if depth == 0: return i + 1
        i += 1
    raise ValueError('unclosed body')

def prune(source, anchors):
    ranges = []
    for anchor in anchors:
        found = source.find(anchor)
        if found < 0: raise ValueError('missing anchor: ' + anchor)
        start = source.index('{', found + len(anchor))
        ranges.append((start, body_end(source, start)))
    for start, end in sorted(ranges, reverse=True):
        source = source[:start] + '{ let _ = writeln!(console, "AUDIT disabled"); }' + source[end:]
    return source

def metrics(elf):
    raw = subprocess.check_output(['nm', '--defined-only', str(elf)], text=True)
    symbols = {}
    for line in raw.splitlines():
        parts = line.split()
        if len(parts) == 3: symbols[parts[2]] = int(parts[0], 16)
    def span(a, b): return symbols[b] - symbols[a]
    return {
        'payload': span('_upper_start', '_upper_load_end'),
        'text': span('_upper_start', '_upper_text_end'),
        'rodata': span('_upper_text_end', '_upper_rodata_end'),
        'data': span('_upper_rodata_end', '_upper_load_end'),
        'bss': span('_bss_start', '_bss_end'),
        'work_free': span('_bss_end', '_stack_bottom'),
        'stack': span('_stack_bottom', '_stack_top'),
        'dma_fixed_actual': span('_dma_fixed_start', '_dma_fixed_end'),
        'spm': span('_entry_start', '_entry_end'),
    }

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--features',default='',help='Cargo features for the baseline, e.g. diagnostics,wifi,software-jpeg')
    parser.add_argument('--variants',nargs='+',choices=VARIANTS,help='selected experiments; baseline is always included')
    args=parser.parse_args()
    selected={name:anchors for name,anchors in VARIANTS.items() if name=='baseline' or not args.variants or name in args.variants}
    OUT.mkdir(parents=True, exist_ok=True)
    original = ROOT / 'target' / TARGET / 'release' / 'esp32-p4'
    report = {'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
              'original_elf_sha256': hashlib.sha256(original.read_bytes()).hexdigest(),
              'features': args.features,
              'original': metrics(original),
              'source_sha256': {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                for name in ['src/main.rs','src/web_server.rs','src/storage/files.rs','Cargo.toml','Cargo.lock','link.x']},
              'variants': {}}
    (OUT / 'symbols.txt').write_text(subprocess.check_output(['nm','-S','--size-sort','--demangle',str(original)], text=True))
    with tempfile.TemporaryDirectory(prefix='p4-memory-audit-') as temp:
        snapshot = Path(temp)
        for directory in ['src', 'vendor', '.cargo']:
            shutil.copytree(ROOT / directory, snapshot / directory)
        for filename in ['Cargo.toml', 'Cargo.lock', 'build.rs', 'link.x', 'commands.tsv', 'command-routes.tsv']:
            shutil.copy2(ROOT / filename, snapshot / filename)
        source = (snapshot / 'src/main.rs').read_text()
        env = dict(os.environ, CARGO_TARGET_DIR=str(OUT / 'build'))
        for name, anchors in selected.items():
            (snapshot / 'src/main.rs').write_text(prune(source, anchors))
            started = time.monotonic()
            with (OUT / (name + '.log')).open('w') as log:
                proc = subprocess.run(['cargo','build','--offline','--release','--locked','--target',TARGET] + (['--features',args.features] if args.features else []),cwd=snapshot,env=env,stdout=log,stderr=subprocess.STDOUT)
            if proc.returncode: raise RuntimeError(name + ' failed: see ' + str(OUT / (name + '.log')))
            elf = OUT / 'build' / TARGET / 'release' / 'esp32-p4'
            result = metrics(elf)
            (OUT / (name + '.symbols.txt')).write_text(subprocess.check_output(['nm','-S','--size-sort','--demangle',str(elf)], text=True))
            result['anchors'] = anchors
            result['build_seconds'] = time.monotonic() - started
            report['variants'][name] = result
            base = report['variants']['baseline']
            result['payload_saved'] = base['payload'] - result['payload']
            result['work_gained'] = result['work_free'] - base['work_free']
            (OUT / 'report.json').write_text(json.dumps(report,indent=2)+'\n')
            print(name, 'payload',result['payload'],'saved',result['payload_saved'],'work',result['work_free'],'gain',result['work_gained'],flush=True)
    assert hashlib.sha256(original.read_bytes()).hexdigest() == report['original_elf_sha256']
    print('DONE: original sources/ELF/firmware not changed; board not flashed',flush=True)

if __name__ == '__main__': main()
