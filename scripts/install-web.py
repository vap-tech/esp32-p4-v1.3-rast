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

    data = args.page.read_bytes()
    try:
        current = request('/fs/www/index.html')
    except HTTPError as error:
        if error.code != 404:
            raise
    else:
        if current == data:
            assert request('/') == data, 'Root is not serving the SD page'
            print('Already installed:', len(data), 'bytes')
            return
        if not args.replace:
            raise SystemExit('Existing /www/index.html differs; use --replace to retain a backup and publish the new page.')
        nonce=uuid.uuid4().hex[:7].upper()
        temp='/www/W'+nonce+'.HTM';backup='/www/B'+nonce+'.BAK'
        request('/fs'+temp, 'PUT', data)
        assert request('/fs'+temp)==data, 'Temporary page readback differs'
        request('/api/rename?path=/www/index.html&name='+backup.rsplit('/',1)[1], 'POST')
        try:
            request('/api/rename?path='+temp+'&name=index.html', 'POST')
        except Exception:
            # CreateNew/rename never overwrites a racing writer's destination.
            try: request('/api/rename?path='+backup+'&name=index.html', 'POST')
            except HTTPError: print('Original preserved at',backup)
            raise
        assert request('/fs/www/index.html')==data and request('/')==data, 'Published page differs; original preserved at '+backup
        print('Installed:',len(data),'bytes; original preserved at',backup)
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
