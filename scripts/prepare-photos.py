#!/usr/bin/env python3
"""Copy originals plus bounded, auto-oriented baseline JPEGs to a board folder.

Requires ImageMagick on the host. Never modifies sources or overwrites SD files.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('--url', default='http://192.168.1.167')
    parser.add_argument('--destination', default='/PHOTOS/100D3100')
    parser.add_argument('--count', type=int, default=10)
    parser.add_argument('--output', type=Path, default=Path('target/photoframe/photos'))
    args = parser.parse_args()
    if not 1 <= args.count <= 64:
        parser.error('--count must be 1..64')
    parts = args.destination.split('/')[1:]
    if not args.destination.startswith('/') or any(p in ('', '.', '..') for p in parts):
        parser.error('--destination must be an absolute directory without empty/dot components')
    sources = sorted(p for p in args.source.iterdir() if p.is_file() and p.suffix.lower() in ('.jpg', '.jpeg'))[:args.count]
    if not sources:
        parser.error('source contains no JPEG files')
    args.output.mkdir(parents=True, exist_ok=True)

    def request(path, method='GET', data=None):
        with urlopen(Request(args.url.rstrip('/') + '/fs' + quote(path, safe='/'), data=data, method=method), timeout=120) as response:
            return response.read()

    path = ''
    for part in [*parts, 'ORIGINAL']:
        path += '/' + part
        try:
            request(path, 'MKCOL')
        except HTTPError as error:
            if error.code != 409:
                raise

    for source in sources:
        prepared = args.output / source.name
        if prepared.resolve() == source.resolve():
            parser.error('--output must not point to the source directory')
        subprocess.run(['magick', str(source), '-auto-orient', '-resize', '1536x1536>',
                        '-background', 'black', '-gravity', 'center', '-extent',
                        '%[fx:ceil(w/8)*8]x%[fx:ceil(h/8)*8]', '-strip', '-interlace',
                        'none', '-sampling-factor', '2x2', '-quality', '90', str(prepared)], check=True)
        for local, destination in [(source, args.destination + '/ORIGINAL/' + source.name),
                                   (prepared, args.destination + '/' + source.name)]:
            data = local.read_bytes()
            try:
                request(destination, 'PUT', data)
            except HTTPError as error:
                if error.code != 409:
                    raise
            actual = request(destination)
            if hashlib.sha256(actual).digest() != hashlib.sha256(data).digest():
                raise SystemExit('Readback differs; existing file preserved: ' + destination)
            print(destination, len(data), 'bytes, SHA256 verified', flush=True)


if __name__ == '__main__':
    main()
