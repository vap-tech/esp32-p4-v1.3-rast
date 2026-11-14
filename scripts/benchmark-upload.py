#!/usr/bin/env python3
"""Measure HTTP PUT/GET with exact readback; delete only our unique test file."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
import uuid
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://192.168.1.167')
    parser.add_argument('--size-mib', type=int, default=64)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if not 1 <= args.size_mib <= 1024:
        parser.error('size must be 1..1024 MiB')
    base = args.url.rstrip('/')

    def request(path, method='GET', data=None):
        with urlopen(Request(base + path, method=method, data=data), timeout=180) as response:
            return response.status, response.read()

    before = json.loads(request('/api/list?path=/')[1])
    name = '/R' + uuid.uuid4().hex[:7].upper() + '.BIN'
    assert name[1:] not in {entry['name'] for entry in before['entries']}
    data = os.urandom(args.size_mib * 1024 * 1024)
    start = time.monotonic()
    assert request('/fs' + name, 'PUT', data)[0] == 201
    write_s = time.monotonic() - start
    try:
        start = time.monotonic()
        received = request('/fs' + name)[1]
        read_s = time.monotonic() - start
        assert received == data, 'SD readback differs'
    finally:
        assert request('/fs' + name, 'DELETE')[0] == 204
    assert json.loads(request('/api/list?path=/')[1]) == before, 'Directory changed during test'
    result = dict(bytes=len(data), write_s=write_s, read_s=read_s,
                  put_mib_s=args.size_mib/write_s, get_mib_s=args.size_mib/read_s,
                  sha256=hashlib.sha256(data).hexdigest(), root_unchanged=True)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2), flush=True)


if __name__ == '__main__':
    main()
