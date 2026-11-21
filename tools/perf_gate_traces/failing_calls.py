"""P50 fixture: calls the I/O parser must count right, run under strace -f.

Two threads, so lines interleave: one writes and syncs a file in a loop,
the other makes calls that fail -- a write to a descriptor opened for
reading (EBADF), a rename of a name that is not there (ENOENT), an fsync
of a pipe (EINVAL) -- and writes to a socket. A connection taken
(accept4) before and after them marks a counted stretch, as a server's do.
"""
import os
import socket
import sys
import threading

d = sys.argv[1]
os.makedirs(d, exist_ok=True)
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(4)
addr = listener.getsockname()


def connect():
    c = socket.create_connection(addr)
    s, _ = listener.accept()
    return c, s


first = connect()
go = threading.Barrier(2)


def writer():
    fd = os.open(os.path.join(d, "data"), os.O_WRONLY | os.O_CREAT, 0o644)
    go.wait()
    for i in range(3):
        os.write(fd, b"x" * (100 + i))
        os.fdatasync(fd)
    os.close(fd)


def failer():
    ro = os.open(os.path.join(d, "data.ro"), os.O_RDONLY | os.O_CREAT, 0o644)
    go.wait()
    try:
        os.write(ro, b"refused")
    except OSError:
        pass
    try:
        os.rename(os.path.join(d, "missing"), os.path.join(d, "renamed"))
    except OSError:
        pass
    r, w = os.pipe()
    try:
        os.fsync(w)
    except OSError:
        pass
    os.write(first[0].fileno(), b"to a socket")
    os.rename(os.path.join(d, "data.ro"), os.path.join(d, "data.kept"))


t = [threading.Thread(target=writer), threading.Thread(target=failer)]
for x in t:
    x.start()
for x in t:
    x.join()
second = connect()
