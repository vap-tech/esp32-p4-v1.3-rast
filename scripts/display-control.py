#!/usr/bin/env python3
"""Control display and SD media over USB Serial/JTAG, without dependencies."""
import argparse
import array
import fcntl
import getpass
import os
import select
import sys
import termios
import time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('commands', nargs='*', help='named commands, quote commands with spaces; help lists commands')
parser.add_argument('--port', default='/dev/ttyACM0')
parser.add_argument('--hold', type=float, default=1.0, help='seconds to observe after each reply')
parser.add_argument('--image', nargs='?', const='HOME.JPG', metavar='PATH', help='show baseline 1024x600 JPEG from SD (default HOME.JPG)')
parser.add_argument('--arbitration', metavar='COMMAND', help='status or bench PROFILE MODE TARGET ROUNDS; memory contention experiment')
parser.add_argument('--record', metavar='PATH', help='record microphone to a new PCM16/16kHz WAV on SD; STOP/ESC finishes')
parser.add_argument('--record-seconds', type=int, default=10, metavar='1..600', help='maximum recording duration (default 10 seconds)')
parser.add_argument('--play', nargs='?', const='SATIE.WAV', metavar='PATH', help='play PCM16/16kHz WAV from SD (default SATIE.WAV); Ctrl-C stops')
parser.add_argument('--seconds', type=float, help='stop playback after this many seconds')
parser.add_argument('--stop', action='store_true', help='stop ongoing SD audio playback')
parser.add_argument('--display-clock', choices=['on','off'], help='enable/disable the RTC clock on framebuffer')
parser.add_argument('--brightness', type=int, choices=range(101), metavar='0..100', help='set backlight percent')
parser.add_argument('--rtc', metavar='COMMAND', help='RTC read/init/set/config/clear/ram/timer/alarm; see docs/rtc.md')
parser.add_argument('--rtc-sync', action='store_true', help='explicitly initialize RTC to laptop UTC, resetting settings/alarms/timer')
parser.add_argument('--sd-files-bench', nargs='?', const=8, type=int, metavar='MiB', help='diagnostics: SRAM to new SD file, verify and remove, 1..64 MiB')
parser.add_argument('--irq-test', action='store_true', help='20ms scoped core0 CLIC/SYSTIMER IRQ test')
parser.add_argument('--worker', choices=['status', 'load on', 'load off', 'pause', 'resume', 'cache-check'], help='HP1 fixed worker control; also available while web is running')
parser.add_argument('--memory', action='store_true', help='report memory/cache configuration')
parser.add_argument('--sd-format-info', action='store_true', help='preview whole-card FAT32 geometry without writing')
parser.add_argument('--format-sd', action='store_true', help='ERASE all SD partitions; quick-format FAT32 and create/verify CONFIG.TXT')
parser.add_argument('--wifi-default', action='store_true', help='connect using firmware built-in network profile, no password prompt')
parser.add_argument('--wifi-connect', metavar='SSID', help='connect C6 STA; asks for password without echo, RAM only')
parser.add_argument('--wifi-password-stdin', action='store_true', help='read Wi-Fi password from stdin instead of terminal prompt')
parser.add_argument('--wifi-seconds', type=int, default=300, help='serve Wi-Fi DHCP/ARP/ping for 1..3600 seconds after DHCP (default 300)')
parser.add_argument('--wifi-scan', action='store_true', help='C6 ESP-Hosted scan; no network credentials or connection')
parser.add_argument('--wifi-probe', nargs='?', const='low', choices=['high', 'low'], help='reset C6 and read SDIO/ESP-Hosted startup; preserves C6 Flash')
parser.add_argument('--eth-probe', action='store_true', help='read PHY IDs/link over MDIO')
parser.add_argument('--wifi-raw', choices=['rx','tx'], help='pure SDIO raw test without Wi-Fi or SD, requires C6 capability')
parser.add_argument('--raw-seconds', type=int, default=10, help='raw test duration 1..30 seconds')
parser.add_argument('--wifi-web', type=int, metavar='SECONDS', help='serve SD HTTP through built-in Wi-Fi profile, 1..3600 seconds')
parser.add_argument('--web-run', nargs='?', const=0, type=int, metavar='SECONDS', help='start/attach web; omitted/0 means persistent, positive seconds diagnostic duration')
parser.add_argument('--eth-run', type=int, metavar='SECONDS', help='poll Ethernet DHCP/ARP/ping for 1..600 seconds (D-cache OFF)')
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
parser.add_argument('--sd-stress', action='store_true', help='read-only dirty RX DMA soak, active framebuffer required')
parser.add_argument('--mixed-stress', action='store_true', help='muted PCM DMA + SD JPEG + LCD soak')
parser.add_argument('--play-stall', nargs='?', const='SATIE.WAV', help='inject underrun; expects owner error and safe cleanup')
parser.add_argument('--wav-lab', action='store_true', help='60s real WAV + SD JPEG + continuous GPIO9 capture (audio-lab build)')
parser.add_argument('--wav-lab-stall', action='store_true', help='inject stall in live WAV lab and verify one underrun')
parser.add_argument('--wav-lab-control', action='store_true', help='8 JPEG + live WAV with RX disabled for overhead comparison')
parser.add_argument('--wav-lab-repeat', action='store_true', help='inject one repeated TX frame and require precise RX detection')
args = parser.parse_args()
if args.seconds is not None and (args.seconds <= 0 or args.play is None):
    parser.error('--seconds requires --play and a positive duration')
if args.web_run is not None and not 0 <= args.web_run <= 86400:
    parser.error('--web-run requires 0..86400 seconds (0=persistent)')
if args.eth_run is not None and not 1 <= args.eth_run <= 600:
    parser.error('--eth-run requires 1..600 seconds')
if not 1 <= args.record_seconds <= 600:
    parser.error('--record-seconds requires 1..600')
for path in (args.record, args.image, args.play, args.jpeg_bench, args.jpeg_profile, args.jpeg_idct_bench, args.play_stall):
    if path is not None and (not path or not path.isascii() or any(c.isspace() for c in path) or len(path) > 80):
        parser.error('media paths must be ASCII without whitespace, at most 80 characters (FAT32 8.3 components)')
if any(not c or not c.isascii() or any(x in c for x in '\r\n') for c in args.commands) or args.hold < 0:
    parser.error('commands must be ASCII names without line breaks; hold must be nonnegative')

def registry(filename):
    return dict(line.split('\t')[:2] for line in (Path(__file__).resolve().parent.parent / filename).read_text().splitlines() if line and not line.startswith('#'))

actions = registry('commands.tsv')
routes = registry('command-routes.tsv')
requirements = {}
for filename in ('commands.tsv', 'command-routes.tsv'):
    for row in (Path(__file__).resolve().parent.parent / filename).read_text().splitlines():
        if row and not row.startswith('#'):
            fields = row.split('\t')
            requirements[fields[0]] = fields[2] if len(fields) > 2 else ''
available_commands = set()
capabilities_known = False

for command in args.commands:
    if command not in actions:
        parser.error('unknown action: ' + command + '; use named actions from commands.tsv or the dedicated options')

def send(fd, payload):
    # Convert dedicated option handlers to the same public namespace as firmware.
    if payload.startswith(b':'):
        line = payload[1:].rstrip(b'\n').decode('ascii')
        is_public = line in actions or any(line == public or line.startswith(public + ' ') for public in routes)
        if not is_public:
            for public, internal in routes.items():
                if line == internal or line.startswith(internal + ' '):
                    line = public + line[len(internal):]
                    break
        candidates = [name for name in (*actions, *routes)
                      if line == name or name in routes and line.startswith(name + ' ')]
        if candidates and capabilities_known:
            name = max(candidates, key=len)
            if name not in available_commands:
                feature = requirements.get(name, '')
                hint = ('; rebuild with --features ' + feature) if feature else ''
                raise SystemExit('Firmware does not advertise ' + name + hint)
        payload = ('\n:' + line + '\n').encode('ascii')
    view = memoryview(payload)
    deadline = time.monotonic() + 5
    while view:
        try:
            n = os.write(fd, view)
            if n == 0:
                raise RuntimeError('USB write returned zero')
            view = view[n:]
        except BlockingIOError:
            if time.monotonic() >= deadline:
                raise TimeoutError('USB not writable for 5 seconds')
            select.select([], [fd], [], 0.1)

wifi_connect_frame = None
if args.wifi_password_stdin and not args.wifi_connect:
    parser.error('--wifi-password-stdin requires --wifi-connect')
if args.wifi_default and args.wifi_connect:
    parser.error('choose --wifi-default or --wifi-connect')
if args.wifi_default and not 1 <= args.wifi_seconds <= 3600:
    parser.error('--wifi-seconds requires 1..3600')
if args.wifi_connect:
    if not 1 <= args.wifi_seconds <= 3600:
        parser.error('--wifi-seconds requires 1..3600')
    ssid = args.wifi_connect.encode('utf-8')
    password = (sys.stdin.readline().rstrip('\r\n') if args.wifi_password_stdin else getpass.getpass('Wi-Fi password: ')).encode('utf-8')
    if not 1 <= len(ssid) <= 32 or not 8 <= len(password) <= 64 or b'\0' in ssid or b'\0' in password:
        parser.error('Wi-Fi SSID/password length or encoding invalid')
    if len(password) == 64 and any(c not in b'0123456789abcdefABCDEF' for c in password):
        parser.error('64-byte password must be hexadecimal PSK')
    wifi_connect_frame = (':wifi connect ' + ssid.hex() + ' ' + password.hex() + ' ' + str(args.wifi_seconds) + '\n').encode('ascii')
    del password

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
                send(fd, b'\x1b')
                stop_at = None
            if select.select([fd], [], [], min(0.1, max(0, deadline - time.monotonic())))[0]:
                data = os.read(fd, 4096)
                if not data:
                    raise RuntimeError('USB disconnected')
                sys.stdout.buffer.write(data)
                sys.stdout.buffer.flush()
                pending.extend(data)
                if until == b'CONSOLE HELP END':
                    boot_errors = {b'!H': 'invalid stage-2 header/layout',
                                   b'!R': 'Flash read failed', b'!C': 'stage-2 CRC mismatch',
                                   b'!W': 'cache writeback timeout', b'!T': 'trap in bootstrap'}
                    reason = boot_errors.get(bytes(pending[:2]))
                    if reason:
                        raise RuntimeError('Bootstrap: ' + reason)
                while b'\n' in pending:
                    line, _, tail = pending.partition(b'\n')
                    pending[:] = tail
                    if until == b'CONSOLE HELP END' and line.startswith(b'  '):
                        name = line.strip().decode('ascii').removesuffix(' [arguments]')
                        available_commands.add(name)
                    if line.startswith(b'COMMAND ERROR') or line.startswith(b'WEB COMMAND unavailable'):
                        raise RuntimeError(line.decode('ascii', errors='replace'))
                    if until is not None and any(marker in line for marker in (until if isinstance(until,tuple) else (until,))):
                        return line
        if until is not None:
            raise TimeoutError('No firmware reply: ' + repr(until))

    if args.stop:
        send(fd, b'\x1b')
        receive(b'MEDIA PLAY DONE', timeout=5)
    send(fd, b':help\n')
    receive(b'CONSOLE HELP END', timeout=20)
    capabilities_known = True
    for command in args.commands:
        print('HOST -> ' + command, flush=True)
        send(fd, (':' + command + '\n').encode('ascii'))
        key = actions[command]
        marker = {'?': b'CONSOLE HELP END', 'q': b'IDLE_COLOR', 'e': b'PANEL_ERRORS', 'L': b'SCAN ', 'B': b'BIST_RESTORED'}.get(key, b'COMMAND ' + command.encode() + b' ')
        receive(marker, timeout=180 if key == 'W' else (40 if key in ('B', 'M', 'm', '#', 'Z', '!', 'p') else 10))
        receive(timeout=args.hold)
    for name, value in [('dcache', args.dcache), ('l2', args.l2)]:
        if value:
            send(fd, (':' + name + ' ' + value + '\n').encode('ascii'))
            line = receive(b'CACHE DONE', timeout=10)
            if b'Err(' in line:
                raise SystemExit('Cache transition failed')
    if args.sd_files_bench is not None:
        if not 1 <= args.sd_files_bench <= 64:
            parser.error('--sd-files-bench requires 1..64 MiB')
        send(fd, (':sd benchmark files ' + str(args.sd_files_bench) + '\n').encode('ascii'))
        line = receive(b'SD FILE BENCH DONE', timeout=args.sd_files_bench * 20 + 60)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('SD file benchmark failed')
    if args.icache:
        send(fd, (':icache ' + args.icache + '\n').encode('ascii'))
        line = receive(b'ICACHE DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('I-cache transition failed')
    if args.display_clock is not None:
        send(fd, (':display clock ' + args.display_clock + '\n').encode('ascii'))
        receive(b'DISPLAY CLOCK DONE', timeout=10)
    if args.brightness is not None:
        send(fd, (':display brightness ' + str(args.brightness) + '\n').encode('ascii'))
        line = receive(b'DISPLAY BRIGHTNESS DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('Brightness command failed')
    if args.rtc_sync or args.rtc is not None:
        import datetime
        if args.rtc_sync and args.rtc is not None:
            raise SystemExit('Choose --rtc or --rtc-sync')
        command = ('init ' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')) if args.rtc_sync else args.rtc
        if not command or not command.isascii() or any(c in command for c in '\r\n') or len(command) > 85:
            raise SystemExit('Invalid RTC command')
        send(fd, (':rtc ' + command + '\n').encode('ascii'))
        line = receive(b'RTC DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('RTC command failed')
    if args.wifi_default:
        wifi_connect_frame = (':wifi default ' + str(args.wifi_seconds) + '\n').encode('ascii')
    if wifi_connect_frame is not None:
        send(fd, wifi_connect_frame)
        wifi_connect_frame = None
        try:
            line = receive(b'C6 DONE', timeout=args.wifi_seconds + 120)
        except KeyboardInterrupt:
            os.write(fd, b'\x1b')
            receive(b'C6 DONE', timeout=60)
            raise SystemExit(130)
        if b'Err(' in line:
            raise SystemExit('C6 connection failed')
    if args.wifi_scan:
        send(fd, b':wifi scan\n')
        try:
            line = receive(b'C6 DONE', timeout=90)
        except KeyboardInterrupt:
            os.write(fd, b'\x1b')
            receive(b'C6 DONE', timeout=45)
            raise SystemExit(130)
        if b'Err(' in line:
            raise SystemExit('C6 scan failed')
    if args.wifi_probe:
        send(fd, (':wifi probe ' + args.wifi_probe + '\n').encode('ascii'))
        try:
            line = receive(b'C6 DONE', timeout=30)
        except KeyboardInterrupt:
            os.write(fd, b'\x1b')
            receive(b'C6 DONE', timeout=30)
            raise SystemExit(130)
        if b'Err(' in line:
            raise SystemExit('C6 probe failed')
    if args.eth_probe:
        send(fd, b':eth-probe\n')
        line = receive(b'ETH PROBE DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('Ethernet PHY probe failed')
    if args.irq_test:
        send(fd,b':system irq test\n'); receive(b'IRQ TEST DONE',timeout=5)
    if args.wifi_raw:
        if not 1 <= args.raw_seconds <= 30: parser.error('--raw-seconds requires 1..30')
        send(fd, (':wifi raw ' + args.wifi_raw + ' ' + str(args.raw_seconds) + '\n').encode('ascii'))
        try:
            line = receive(b'C6 RAW DONE', timeout=args.raw_seconds + 25)
        except KeyboardInterrupt:
            send(fd,b'\x1b'); line = receive(b'C6 RAW DONE', timeout=15)
        if b'Err(' in line or b'guard_ok: false' in line: raise SystemExit('Raw SDIO test failed')
    if args.wifi_web is not None:
        if not 1 <= args.wifi_web <= 3600:
            parser.error('--wifi-web requires 1..3600 seconds')
        send(fd, (':net wifi serve ' + str(args.wifi_web) + '\n').encode('ascii'))
        try:
            line = receive(b'WEB RUN DONE', timeout=args.wifi_web + 60)
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'WEB RUN DONE', timeout=30)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('Wi-Fi web server failed')
    if args.web_run is not None:
        send(fd, (':web-run ' + str(args.web_run) + '\n').encode('ascii'))
        try:
            line = receive((b'WEB RUN DONE',b'WEB RUN ACTIVE'), timeout=args.web_run + 30)
            if b'WEB RUN ACTIVE' in line:
                while True:
                    try:
                        line=receive(b'WEB RUN DONE', timeout=3600)
                        break
                    except TimeoutError:
                        pass
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'WEB RUN DONE', timeout=10)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('Web server failed')
    if args.eth_run is not None:
        send(fd, (':eth-run ' + str(args.eth_run) + '\n').encode('ascii'))
        try:
            line = receive(b'ETH RUN DONE', timeout=args.eth_run + 15)
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'ETH RUN DONE', timeout=5)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('Ethernet run failed')
    if args.sd_format_info or args.format_sd:
        send(fd, b':format-sd FAT32 ERASE\n' if args.format_sd else b':format-sd\n')
        line = receive(b'SD FORMAT DONE', timeout=300)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('SD format failed')
    if args.l2_probe:
        send(fd, b':l2-probe\n')
        line = receive(b'L2 PROBE DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('L2 probe failed')
    if args.mixed_stress:
        send(fd, b':mixed-stress\n')
        line = receive(b'MIXED DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line or b'guard_ok: false' in line:
            raise SystemExit('Mixed stress failed')
    if args.play_stall:
        send(fd, (':play-stall ' + args.play_stall + '\n').encode('ascii'))
        line = receive(b'MEDIA PLAY DONE', timeout=10)
        if b'Err(Audio(I2s(Dma(' not in line:
            raise SystemExit('Expected underrun DMA owner error')
    if args.sd_stress:
        send(fd, b':sd-stress\n')
        line = receive(b'SD STRESS DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line or b'guard_ok: false' in line:
            raise SystemExit('SD stress failed')
    if args.clock:
        send(fd, b':clock\n')
        receive(b'CLOCK DONE', timeout=10)
    if args.worker is not None:
        send(fd, (':system worker ' + args.worker + '\n').encode('ascii'))
        line = receive(b'HP WORKER state=', timeout=5)
        if b'errors=0' not in line or b'trap=00000000/00000000/00000000' not in line:
            raise SystemExit('HP1 worker error; see firmware reply')
        if args.worker == 'pause' and b'state=2 ' not in line:
            raise SystemExit('HP1 pause was not acknowledged')
        if args.worker == 'resume' and b'state=1 ' not in line:
            raise SystemExit('HP1 resume was not acknowledged')
    if args.memory:
        send(fd, b':memory\n')
        receive(b'MEMORY DONE', timeout=10)
    if args.memory_test:
        send(fd, b':memory-test\n')
        line = receive(b'MEMORY DONE', timeout=180)
        if b'Err(' in line:
            raise RuntimeError('Memory test failed; see firmware reply')
    if args.memory_bench:
        send(fd, b':memory-bench\n')
        line = receive(b'MEMORY DONE', timeout=180)
        if b'Err(' in line:
            raise RuntimeError('Memory benchmark failed; see firmware reply')
    if args.image is not None:
        print('HOST -> image ' + args.image, flush=True)
        send(fd, (':image ' + args.image + '\n').encode('ascii'))
        line = receive(b'MEDIA IMAGE DONE', timeout=120)
        if b'Err(' in line or b'active=false' in line:
            raise RuntimeError('Image command failed; see firmware reply')
    if args.jpeg_bench is not None:
        print('HOST -> jpeg-bench ' + args.jpeg_bench, flush=True)
        send(fd, (':jpeg-bench ' + args.jpeg_bench + '\n').encode('ascii'))
        line = receive(b'JPEG BENCH DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('JPEG benchmark failed; see firmware reply')
    if args.jpeg_profile is not None:
        print('HOST -> jpeg-profile ' + args.jpeg_profile, flush=True)
        send(fd, (':jpeg-profile ' + args.jpeg_profile + '\n').encode('ascii'))
        line = receive(b'JPEG PROFILE DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('JPEG profile failed; see firmware reply')
    for enabled, command in [(args.wav_lab, 'wav-lab'), (args.wav_lab_stall, 'wav-lab-stall'), (args.wav_lab_control, 'wav-lab-control'), (args.wav_lab_repeat, 'wav-lab-repeat')]:
        if enabled:
            send(fd, (':' + command + '\n').encode('ascii'))
            try:
                line = receive(b'WAV LAB DONE', timeout=180)
            except KeyboardInterrupt:
                send(fd, b'\x1b')
                line = receive(b'WAV LAB DONE', timeout=10)
            if b'Err(' in line or b'guards=false' in line or b'guard_ok: false' in line:
                raise SystemExit('WAV lab failed')
    if args.jpeg_idct_bench is not None:
        print('HOST -> jpeg-idct-bench ' + args.jpeg_idct_bench, flush=True)
        send(fd, (':jpeg-idct-bench ' + args.jpeg_idct_bench + '\n').encode('ascii'))
        line = receive(b'JPEG IDCT DONE', timeout=180)
        if b'Err(' in line or b'guards=false' in line:
            raise RuntimeError('IDCT benchmark failed; see firmware reply')
    if args.arbitration is not None:
        command = args.arbitration
        if not command or not command.isascii() or any(c in command for c in '\r\n') or len(command) > 72:
            raise SystemExit('Invalid arbitration command')
        send(fd, (':memory arbitration ' + command + '\n').encode('ascii'))
        try:
            line = receive(b'ARBITRATION DONE', timeout=180)
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'ARBITRATION DONE', timeout=10)
        if b'Err(' in line:
            raise SystemExit('Arbitration experiment failed')
    if args.record is not None:
        print('HOST -> record ' + args.record, flush=True)
        send(fd, (':audio record ' + args.record + ' ' + str(args.record_seconds) + '\n').encode('ascii'))
        try:
            line = receive(b'RECORD DONE', timeout=args.record_seconds + 30)
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'RECORD DONE', timeout=15)
        if b'Err(' in line or b'guard_ok: false' in line:
            raise SystemExit('Recording failed; see firmware reply')
    if args.play is not None:
        print('HOST -> play ' + args.play, flush=True)
        send(fd, (':play ' + args.play + '\n').encode('ascii'))
        try:
            line = receive(b'MEDIA PLAY DONE', timeout=86400, stop_after=args.seconds)
        except KeyboardInterrupt:
            send(fd, b'\x1b')
            line = receive(b'MEDIA PLAY DONE', timeout=5)
        if b'Err(' in line:
            raise RuntimeError('Playback failed; see firmware reply')
finally:
    os.close(fd)
