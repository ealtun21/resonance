"""Drive a QEMU guest's console over QMP: screenshot, keys, text, mouse. Stdlib only.

  vmgui.py QMP_SOCK shot OUT.png
  vmgui.py QMP_SOCK key  meta_l+spc ret      (qcodes joined with + press together)
  vmgui.py QMP_SOCK type 'some text'
  vmgui.py QMP_SOCK click X Y [N] [right]    (pixels of the 1920x1080 framebuffer)
"""
import json
import os
import socket
import sys
import time

W, H = 1920, 1080
SHIFTED = {'_': '-', ':': ';', '"': "'", '|': '\\', '>': '.', '<': ',', '(': '9', ')': '0', '*': '8',
           '$': '4', '&': '7', '@': '2', '%': '5', '#': '3', '!': '1', '+': '=', '{': '[', '}': ']',
           '?': '/', '^': '6', '~': '`'}
PLAIN = {' ': 'spc', '\\': 'backslash', '.': 'dot', ',': 'comma', '-': 'minus', ';': 'semicolon',
         '/': 'slash', "'": 'apostrophe', '=': 'equal', '[': 'bracket_left', ']': 'bracket_right',
         '`': 'grave_accent', '\n': 'ret', '\t': 'tab'}


class Qmp:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(path)
        self.f = self.s.makefile('rw')
        self.f.readline()
        self.call('qmp_capabilities')

    def call(self, cmd, **args):
        self.f.write(json.dumps({'execute': cmd, 'arguments': args}) + '\n')
        self.f.flush()
        while True:  # skip async events
            r = json.loads(self.f.readline())
            if 'return' in r or 'error' in r:
                return r


def qcode(ch):
    if ch in PLAIN:
        return [PLAIN[ch]]
    if ch in SHIFTED:
        base = SHIFTED[ch]
        return ['shift', PLAIN.get(base, base)]
    if ch.isupper():
        return ['shift', ch.lower()]
    return [ch]


def keys(q, codes, hold=0.03):
    ev = [{'type': 'key', 'data': {'down': True, 'key': {'type': 'qcode', 'data': c}}} for c in codes]
    q.call('input-send-event', events=ev)
    time.sleep(hold)
    q.call('input-send-event', events=[dict(e, data=dict(e['data'], down=False)) for e in reversed(ev)])
    time.sleep(hold)


def main():
    q = Qmp(sys.argv[1])
    cmd = sys.argv[2]
    if cmd == 'shot':
        raw = os.path.abspath(sys.argv[3]) + '.ppm'
        r = q.call('screendump', filename=raw, format='ppm')
        if 'error' in r:
            sys.exit(r)
        time.sleep(0.5)
        try:
            from PIL import Image
            Image.open(raw).save(sys.argv[3])
            os.unlink(raw)
        except ImportError:  # no PIL: keep the ppm
            print(raw)
    elif cmd == 'key':
        for chord in sys.argv[3:]:
            keys(q, chord.split('+'))
    elif cmd == 'type':
        for ch in sys.argv[3]:
            keys(q, qcode(ch))
    elif cmd == 'click':
        x, y = int(sys.argv[3]), int(sys.argv[4])
        n = int(sys.argv[5]) if len(sys.argv) > 5 and sys.argv[5].isdigit() else 1
        btn = 'right' if 'right' in sys.argv[5:] else 'left'
        ax = lambda a, v: {'type': 'abs', 'data': {'axis': a, 'value': v}}
        q.call('input-send-event', events=[ax('x', int(x / W * 32767)), ax('y', int(y / H * 32767))])
        time.sleep(0.3)
        for _ in range(n):
            for down in (True, False):
                q.call('input-send-event', events=[{'type': 'btn', 'data': {'down': down, 'button': btn}}])
                time.sleep(0.08)
            time.sleep(0.15)


main()
