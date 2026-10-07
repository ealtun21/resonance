import socket, sys, time

S = sys.argv[1]


def send(cmds):
    s = socket.socket(socket.AF_UNIX)
    s.connect(S)
    s.settimeout(1)
    try:
        s.recv(4096)
    except Exception:
        pass
    for c in cmds:
        s.sendall((c + "\n").encode())
        time.sleep(0.06)
        try:
            s.recv(4096)
        except Exception:
            pass
    s.close()


m = {' ': 'spc', '\\': 'backslash', '.': 'dot', ',': 'comma', '-': 'minus', '_': 'shift-minus',
     ':': 'shift-semicolon', ';': 'semicolon', '/': 'slash', '"': 'shift-apostrophe', "'": 'apostrophe',
     '|': 'shift-backslash', '>': 'shift-dot', '<': 'shift-comma', '=': 'equal', '(': 'shift-9',
     ')': 'shift-0', '*': 'shift-8', '$': 'shift-4', '&': 'shift-7', '@': 'shift-2', '%': 'shift-5',
     '#': 'shift-3', '!': 'shift-1', '+': 'shift-equal', '{': 'shift-bracket_left',
     '}': 'shift-bracket_right', '[': 'bracket_left', ']': 'bracket_right', '?': 'shift-slash'}


def keys(text):
    out = []
    for ch in text:
        if ch in m:
            out.append('sendkey ' + m[ch])
        elif ch.isupper():
            out.append('sendkey shift-' + ch.lower())
        else:
            out.append('sendkey ' + ch)
    return out


if sys.argv[2] == 'type':
    send(keys(sys.argv[3]))
else:
    send(['sendkey ' + k for k in sys.argv[3:]])
