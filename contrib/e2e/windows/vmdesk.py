#!/usr/bin/env python3
"""Drive a quickemu guest's desktop over QMP (screenshots, mouse, keyboard).

    vmdesk.py SOCK shot OUT.png        full-screen screenshot
    vmdesk.py SOCK click X Y [right|double]   move + click (guest pixels)
    vmdesk.py SOCK move X Y
    vmdesk.py SOCK type 'text'          type text
    vmdesk.py SOCK key ctrl-alt-delete  one key combo (QEMU key names)

SOCK is the unix socket passed to qemu as `-qmp unix:SOCK,server,nowait`. QMP absolute
events are normalised to 0..32767, so coordinates are scaled by the screenshot size.
This is real (emulated-hardware) input, so it reaches winit/egui windows where
SendInput from a scheduled task does not.
"""
import json, os, socket, subprocess, sys, tempfile, time

CHARS = {' ': 'spc', '\\': 'backslash', '.': 'dot', ',': 'comma', '-': 'minus', '_': 'shift-minus',
         ':': 'shift-semicolon', ';': 'semicolon', '/': 'slash', '"': 'shift-apostrophe',
         "'": 'apostrophe', '|': 'shift-backslash', '>': 'shift-dot', '<': 'shift-comma',
         '=': 'equal', '(': 'shift-9', ')': 'shift-0', '*': 'shift-8', '$': 'shift-4',
         '&': 'shift-7', '@': 'shift-2', '%': 'shift-5', '#': 'shift-3', '!': 'shift-1',
         '+': 'shift-equal', '{': 'shift-bracket_left', '}': 'shift-bracket_right',
         '[': 'bracket_left', ']': 'bracket_right', '?': 'shift-slash', '\n': 'ret'}


class Qmp:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(path)
        self.f = self.s.makefile('rw')
        self.f.readline()
        self.cmd('qmp_capabilities')

    def cmd(self, name, **args):
        self.f.write(json.dumps({'execute': name, 'arguments': args}) + '\n')
        self.f.flush()
        while True:
            r = json.loads(self.f.readline())
            if 'return' in r or 'error' in r:
                return r

    def events(self, evs):
        return self.cmd('input-send-event', events=evs)

    def shot(self, out):
        ppm = os.path.join(tempfile.mkdtemp(), 's.ppm')
        self.cmd('screendump', filename=ppm)
        for _ in range(50):
            if os.path.exists(ppm) and os.path.getsize(ppm) > 0:
                time.sleep(0.2)
                break
            time.sleep(0.1)
        subprocess.run(['magick', ppm, out], check=True)
        return subprocess.run(['magick', 'identify', '-format', '%w %h', out],
                              capture_output=True, text=True).stdout.split()

    def move(self, x, y, size):
        w, h = size
        ax = lambda a, v: {'type': 'abs', 'data': {'axis': a, 'value': v}}
        self.events([ax('x', int(x / w * 32767)), ax('y', int(y / h * 32767))])

    def button(self, name, down):
        self.events([{'type': 'btn', 'data': {'down': down, 'button': name}}])

    def key(self, combo):
        keys = [{'type': 'qcode', 'data': k} for k in combo.split('-')]
        self.events([{'type': 'key', 'data': {'down': True, 'key': k}} for k in keys])
        time.sleep(0.05)
        self.events([{'type': 'key', 'data': {'down': False, 'key': k}} for k in reversed(keys)])


def main():
    sock, verb, *a = sys.argv[1:]
    q = Qmp(sock)
    if verb == 'shot':
        q.shot(a[0])
        return
    if verb in ('click', 'move'):
        # screen size from a throwaway screendump
        size = [int(v) for v in q.shot(tempfile.mktemp(suffix='.png'))]
        q.move(int(a[0]), int(a[1]), size)
        time.sleep(0.3)
        if verb == 'click':
            btn = 'right' if 'right' in a[2:] else 'left'
            for _ in range(2 if 'double' in a[2:] else 1):
                q.button(btn, True)
                time.sleep(0.08)
                q.button(btn, False)
                time.sleep(0.15)
    elif verb == 'type':
        for ch in a[0]:
            q.key(CHARS.get(ch, ('shift-' + ch.lower()) if ch.isupper() else ch))
            time.sleep(0.03)
    elif verb == 'key':
        q.key(a[0])


main()
