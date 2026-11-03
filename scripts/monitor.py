#!/usr/bin/env python3
"""Linux USB Serial/JTAG monitor; release DTR/RTS instead of entering download."""
import array
import fcntl
import os
import select
import sys
import termios
import time

port = sys.argv[1] if len(sys.argv) > 1 else "/dev/ttyACM0"
fd = None
try:
    while True:
        try:
            if fd is None:
                fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
                cfg = termios.tcgetattr(fd)
                cfg[0] = cfg[1] = cfg[3] = 0
                cfg[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
                cfg[4] = cfg[5] = termios.B115200
                cfg[6][termios.VMIN] = 0
                cfg[6][termios.VTIME] = 0
                termios.tcsetattr(fd, termios.TCSANOW, cfg)
                fcntl.ioctl(fd, termios.TIOCMBIC,
                            array.array("i", [termios.TIOCM_DTR | termios.TIOCM_RTS]))
            if select.select([fd], [], [], 0.5)[0]:
                data = os.read(fd, 4096)
                if not data:
                    raise OSError("USB disconnected")
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()
        except (OSError, termios.error):
            if fd is not None:
                os.close(fd)
                fd = None
            time.sleep(0.2)
except KeyboardInterrupt:
    pass
finally:
    if fd is not None:
        os.close(fd)
