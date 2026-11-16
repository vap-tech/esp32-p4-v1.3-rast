#!/usr/bin/env python3
"""Install editable /www/index.html without reflashing or replacing user files."""
import argparse
import hashlib
import uuid
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://192.168.1.167')
    parser.add_argument('--page', type=Path, default=Path(__file__).resolve().parents[1] / 'frontend/index.html')
    parser.add_argument('--replace', action='store_true', help='publish a verified temporary page; keep the old page as a .BAK file')
    args = parser.parse_args()
    base = args.url.rstrip('/')

    def request(path, method='GET', data=None):
        with urlopen(Request(base + path, data=data, method=method), timeout=120) as response:
            return response.read()

    def publish(name, data):
        path = '/www/' + name
        try:
            current = request('/fs' + path)
        except HTTPError as error:
            if error.code != 404:
                raise
            current = None
        if current == data:
            print('Already installed:', path, len(data), 'bytes')
            return
        if current is not None and not args.replace:
            raise SystemExit('Existing ' + path + ' differs; use --replace to retain a backup.')
        nonce = uuid.uuid4().hex[:7].upper()
        temp = '/www/W' + nonce + '.TMP'
        backup = '/www/B' + nonce + '.BAK'
        request('/fs' + temp, 'PUT', data)
        assert request('/fs' + temp) == data, 'Temporary asset readback differs'
        if current is not None:
            request('/api/rename?path=' + path + '&name=' + backup.rsplit('/', 1)[1], 'POST')
        try:
            request('/api/rename?path=' + temp + '&name=' + name, 'POST')
        except Exception:
            if current is not None:
                try:
                    request('/api/rename?path=' + backup + '&name=' + name, 'POST')
                except HTTPError:
                    print('Original preserved at', backup)
            raise
        assert request('/fs' + path) == data, 'Published asset differs: ' + path
        print('Installed:', path, len(data), 'bytes; SHA256', hashlib.sha256(data).hexdigest())
        if current is not None:
            print('Original preserved at', backup)

    try:
        request('/fs/www', 'MKCOL')
    except HTTPError as error:
        if error.code != 409:
            raise
    # Publish the helper first: an updated page must never reference a missing asset.
    helper = args.page.parent / 'photo-tools.js'
    if helper.exists():
        publish(helper.name, helper.read_bytes())
    publish('index.html', args.page.read_bytes())
    assert request('/') == args.page.read_bytes(), 'Root is not serving the SD page'


if __name__ == '__main__':
    main()
