#!/usr/bin/env python3
"""Install editable /www/index.html without reflashing or replacing user files."""
import argparse
import hashlib
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://192.168.1.167')
    parser.add_argument('--page', type=Path, default=Path(__file__).resolve().parents[1] / 'frontend/index.html')
    args = parser.parse_args()
    base = args.url.rstrip('/')

    def request(path, method='GET', data=None):
        with urlopen(Request(base + path, data=data, method=method), timeout=120) as response:
            return response.read()

    data = args.page.read_bytes()
    try:
        current = request('/fs/www/index.html')
    except HTTPError as error:
        if error.code != 404:
            raise
    else:
        if current != data:
            raise SystemExit('Existing /www/index.html differs; preserve or rename it before installing.')
        print('Already installed:', len(data), 'bytes')
        return
    try:
        request('/fs/www', 'MKCOL')
    except HTTPError as error:
        if error.code != 409:
            raise
    request('/fs/www/index.html', 'PUT', data)
    assert request('/fs/www/index.html') == data, 'SD readback differs'
    assert request('/') == data, 'Root is not serving the SD page'
    print('Installed /www/index.html:', len(data), 'bytes; SHA256', hashlib.sha256(data).hexdigest())


if __name__ == '__main__':
    main()
