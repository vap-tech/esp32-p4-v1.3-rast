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
parser.add_argument('--memory', action='store_true', help='report memory/cache configuration')
parser.add_argument('--memory-test', action='store_true', help='destructive full PSRAM test before framebuffer initialization')
parser.add_argument('--memory-bench', action='store_true', help='benchmark SRAM/PSRAM scratch buffers; preserves display pixels')
parser.add_argument('--jpeg-bench', nargs='?', const='HOME.JPG', metavar='PATH', help='compare JPEG decode with framebuffer writes vs checksum only (ABBA)')
parser.add_argument('--jpeg-profile', nargs='?', const='HOME.JPG', metavar='PATH', help='profile JPEG stages vs uninstrumented controls (PUUP)')
parser.add_argument('--clock', action='store_true', help='read CPU root/dividers and calibrate mcycle without changing frequency')
parser.add_argument('--jpeg-idct-bench', nargs='?', const='HOME.JPG', metavar='PATH', help='compare Loeffler vs reference matrix IDCT (LMML)')
parser.add_argument('--icache', choices=['on', 'off'], help='toggle core0 I-cache only; D-cache/L2 stay bypassed')
parser.add_argument('--dcache', choices=['on', 'off'], help='experimental D-cache toggle, L2=0 only')
parser.add_argument('--l2', choices=['on', 'off'], help='experimental L2 toggle, separately linked 128KiB layout only')
parser.add_argument('--l2-probe', action='store_true', help='read-only L2 positive control on initialized framebuffer')
args = parser.parse_args()
if args.seconds is not None and (args.seconds <= 0 or args.play is None):
    parser.error('--seconds requires --play and a positive duration')
for path in (args.image, args.play, args.jpeg_bench, args.jpeg_profile, args.jpeg_idct_bench):
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
    for name, value in [('dcache', args.dcache), ('l2', args.l2)]:
        if value:
            os.write(fd, (':' + name + ' ' + value + '\n').encode('ascii'))
            line = receive(b'CACHE DONE', timeout=10)
            if b'Err(' in line:
                raise SystemExit('Cache transition failed')
    if args.icache:
        os.write(fd, (':icache ' + args.icache + '\n').encode('ascii'))
        line = receive(b'ICACHE DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('I-cache transition failed')
    if args.l2_probe:
        os.write(fd, b':l2-probe\n')
        line = receive(b'L2 PROBE DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('L2 probe failed')
    if args.clock:
        os.write(fd, b':clock\n')
        receive(b'CLOCK DONE', timeout=10)
    if args.memory:
        os.write(fd, b':memory\n')
        receive(b'MEMORY DONE', timeout=10)
    if args.memory_test:
        os.write(fd, b':memory-test\n')
        line = receive(b'MEMORY DONE', timeout=180)
        if b'Err(' in line:
            raise RuntimeError('Memory test failed; see firmware reply')
    if args.memory_bench:
        os.write(fd, b':memory-bench\n')
        line = receive(b'MEMORY DONE', timeout=180)
        if b'Err(' in line:
            raise RuntimeError('Memory benchmark failed; see firmware reply')
    if args.image is not None:
        print('HOST -> image ' + args.image, flush=True)
        os.write(fd, (':image ' + args.image + '\n').encode('ascii'))
        line = receive(b'MEDIA IMAGE DONE', timeout=120)
        if b'Err(' in line or b'active=false' in line:
            raise RuntimeError('Image command failed; see firmware reply')
    if args.jpeg_bench is not None:
        print('HOST -> jpeg-bench ' + args.jpeg_bench, flush=True)
        os.write(fd, (':jpeg-bench ' + args.jpeg_bench + '\n').encode('ascii'))
        line = receive(b'JPEG BENCH DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('JPEG benchmark failed; see firmware reply')
    if args.jpeg_profile is not None:
        print('HOST -> jpeg-profile ' + args.jpeg_profile, flush=True)
        os.write(fd, (':jpeg-profile ' + args.jpeg_profile + '\n').encode('ascii'))
        line = receive(b'JPEG PROFILE DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('JPEG profile failed; see firmware reply')
    if args.jpeg_idct_bench is not None:
        print('HOST -> jpeg-idct-bench ' + args.jpeg_idct_bench, flush=True)
        os.write(fd, (':jpeg-idct-bench ' + args.jpeg_idct_bench + '\n').encode('ascii'))
        line = receive(b'JPEG IDCT DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('IDCT benchmark failed; see firmware reply')
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
