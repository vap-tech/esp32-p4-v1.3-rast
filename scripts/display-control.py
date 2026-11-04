#!/usr/bin/env python3
"""Send JD9165 bring-up commands over Linux USB Serial/JTAG, without dependencies."""
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
args = parser.parse_args()
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

    def receive(until=None, timeout=10.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
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
                        return
        if until is not None:
            raise TimeoutError('No firmware reply: ' + until.decode())

    os.write(fd, b'?')
    receive(b'DISPLAY commands:', timeout=20)
    for command in args.commands:
        print('HOST -> ' + command, flush=True)
        os.write(fd, command.encode('ascii'))
        marker = {'?': b'DISPLAY commands:', 'q': b'IDLE_COLOR', 'e': b'PANEL_ERRORS', 'L': b'SCAN '}.get(command, b'CMD ' + command.encode() + b' ')
        receive(marker)
        receive(timeout=args.hold)
finally:
    os.close(fd)
