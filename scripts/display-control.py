#!/usr/bin/env python3
"""Control display and SD media over USB Serial/JTAG, without dependencies."""
import argparse
import array
import fcntl
import os
import select
import sys
import termios
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('commands', nargs='*', help='one-character commands; ? prints firmware help')
parser.add_argument('--port', default='/dev/ttyACM0')
parser.add_argument('--hold', type=float, default=1.0, help='seconds to observe after each reply')
parser.add_argument('--image', nargs='?', const='HOME.JPG', metavar='PATH', help='show baseline 1024x600 JPEG from SD (default HOME.JPG)')
parser.add_argument('--play', nargs='?', const='SATIE.WAV', metavar='PATH', help='play PCM16/16kHz WAV from SD (default SATIE.WAV); Ctrl-C stops')
parser.add_argument('--seconds', type=float, help='stop playback after this many seconds')
parser.add_argument('--stop', action='store_true', help='stop ongoing SD audio playback')
args = parser.parse_args()
if args.seconds is not None and (args.seconds <= 0 or args.play is None):
    parser.error('--seconds requires --play and a positive duration')
for path in (args.image, args.play):
    if path is not None and (not path or not path.isascii() or any(c.isspace() for c in path) or len(path) > 80):
        parser.error('media paths must be ASCII without whitespace, at most 80 characters (FAT32 8.3 components)')
if any(len(c) != 1 or not c.isascii() for c in args.commands) or args.hold < 0:
    parser.error('commands must be single ASCII characters and hold must be nonnegative')

fd = os.open(args.port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
try:
    cfg = termios.tcgetattr(fd)
    cfg[0] = cfg[1] = cfg[3] = 0
    cfg[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    cfg[4] = cfg[5] = termios.B115200
    cfg[6][termios.VMIN] = cfg[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, cfg)
    fcntl.ioctl(fd, termios.TIOCMBIC, array.array('i', [termios.TIOCM_DTR | termios.TIOCM_RTS]))
    pending = bytearray()

    def receive(until=None, timeout=10.0, stop_after=None):
        deadline = time.monotonic() + timeout
        stop_at = time.monotonic() + stop_after if stop_after is not None else None
        while time.monotonic() < deadline:
            if stop_at is not None and time.monotonic() >= stop_at:
                os.write(fd, b'\x1b')
                stop_at = None
            if select.select([fd], [], [], min(0.1, max(0, deadline - time.monotonic())))[0]:
                data = os.read(fd, 4096)
                if not data:
                    raise RuntimeError('USB disconnected')
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()
                pending.extend(data)
                while b'\n' in pending:
                    line, _, tail = pending.partition(b'\n')
                    pending[:] = tail
                    if until is not None and until in line:
                        return line
        if until is not None:
            raise TimeoutError('No firmware reply: ' + until.decode())

    if args.stop:
        os.write(fd, b'\x1b')
        receive(b'MEDIA PLAY DONE', timeout=5)
    os.write(fd, b'?')
    receive(b'DISPLAY commands:', timeout=20)
    for command in args.commands:
        print('HOST -> ' + command, flush=True)
        os.write(fd, command.encode('ascii'))
        marker = {'?': b'DISPLAY commands:', 'q': b'IDLE_COLOR', 'e': b'PANEL_ERRORS', 'L': b'SCAN ', 'B': b'BIST_RESTORED'}.get(command, b'CMD ' + command.encode() + b' ')
        receive(marker, timeout=180 if command == 'W' else (40 if command in ('B', 'M', 'm', '#', 'Z', '!', 'p') else 10))
        receive(timeout=args.hold)
    if args.image is not None:
        print('HOST -> image ' + args.image, flush=True)
        os.write(fd, (':image ' + args.image + '\n').encode('ascii'))
        line = receive(b'MEDIA IMAGE DONE', timeout=120)
        if b'Err(' in line or b'active=false' in line:
            raise RuntimeError('Image command failed; see firmware reply')
    if args.play is not None:
        print('HOST -> play ' + args.play, flush=True)
        os.write(fd, (':play ' + args.play + '\n').encode('ascii'))
        try:
            line = receive(b'MEDIA PLAY DONE', timeout=86400, stop_after=args.seconds)
        except KeyboardInterrupt:
            os.write(fd, b'\x1b')
            line = receive(b'MEDIA PLAY DONE', timeout=5)
        if b'Err(' in line:
            raise RuntimeError('Playback failed; see firmware reply')
finally:
    os.close(fd)
