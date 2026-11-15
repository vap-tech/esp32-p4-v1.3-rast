#!/usr/bin/env python3
"""RAM-only HTTP GET/POST timing; verify GET pattern and POST byte count."""
import argparse
import json
import os
from pathlib import Path
import statistics
import time
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://192.168.1.167')
    parser.add_argument('--size-mib', type=int, default=4, choices=range(1, 5))
    parser.add_argument('--rounds', type=int, default=7)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 100:
        parser.error('rounds must be 1..100')
    size = args.size_mib * 1024 * 1024
    expected = bytes(range(256)) * (size // 256)
    upload = os.urandom(size)
    url = args.url.rstrip('/') + '/api/net-test?bytes=' + str(size)
    results = []
    for trial in range(args.rounds + 1):
        row = {'trial': trial, 'bytes': size}
        for method in (['GET', 'POST'] if trial % 2 == 0 else ['POST', 'GET']):
            req = Request(url, data=upload if method == 'POST' else None,
                          method=method, headers={'Connection': 'close'})
            start = time.monotonic()
            with urlopen(req, timeout=60) as response:
                data = response.read()
                assert response.status == 200
            elapsed = time.monotonic() - start
            if method == 'GET':
                assert data == expected, 'RAM GET differs'
            else:
                assert json.loads(data)['received'] == size, 'RAM POST byte count differs'
            row[method.lower() + '_s'] = elapsed
            row[method.lower() + '_mib_s'] = args.size_mib / elapsed
            time.sleep(0.1)
        print(json.dumps(row), flush=True)
        if trial:  # First pair warms up the network; it is never in the median.
            results.append(row)
    summary = {'trials': results, 'get_mib_s': statistics.median(r['get_mib_s'] for r in results),
               'post_mib_s': statistics.median(r['post_mib_s'] for r in results),
               'get_pattern_exact': True, 'post_byte_count_exact': True}
    if args.output:
        args.output.write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    main()
