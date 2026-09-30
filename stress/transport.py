"""Read-only Linux UNIX_DIAG measurements, independent of mux's protocol.

UAPI: linux/unix_diag.h, linux/sock_diag.h. Kernel net/unix/diag.c supplies
receive/send queues and SK_MEMINFO_WMEM_ALLOC/SNDBUF. No mux state is queried.
"""
import os
from pathlib import Path
import socket
import struct


def socket_inodes(pid):
    result = []
    for fd in (Path('/proc') / str(pid) / 'fd').iterdir():
        try:
            target = os.readlink(fd)
        except FileNotFoundError:
            continue
        if target.startswith('socket:['):
            result.append(int(target[8:-1]))
    return result


def diagnose(inode):
    request = struct.pack('=BBHIIIII', socket.AF_UNIX, 0, 0, 0xffffffff,
                          inode, 0x04 | 0x10 | 0x20, 0xffffffff, 0xffffffff)
    with socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 4) as sock:
        sock.settimeout(2)
        sock.sendto(struct.pack('=IHHII', 16+len(request), 20, 1, 1, 0)+request, (0, 0))
        data = sock.recv(65536)
    length, kind, _, _, _ = struct.unpack_from('=IHHII', data)
    if kind == 2:
        error, = struct.unpack_from('=i', data, 16)
        raise OSError(-error, os.strerror(-error))
    assert kind == 20 and length <= len(data), 'unexpected UNIX_DIAG reply'
    actual, = struct.unpack_from('=I', data, 20)
    assert actual == inode, 'UNIX_DIAG returned another socket'
    result = {'inode': inode}
    offset = 32
    while offset < length:
        size, attribute = struct.unpack_from('=HH', data, offset)
        assert size >= 4 and offset+size <= length, 'invalid UNIX_DIAG attribute'
        value = data[offset+4:offset+size]
        if attribute == 2:
            result['peer'], = struct.unpack('=I', value)
        elif attribute == 4:
            result['receive_bytes'], result['send_bytes'] = struct.unpack('=II', value)
        elif attribute == 5:
            mem = struct.unpack('='+('I'*(len(value)//4)), value)
            result.update(receive_memory=mem[0], receive_buffer=mem[1],
                          send_memory=mem[2], send_buffer=mem[3])
        offset += (size+3) & ~3
    return result


def client_transport(pid, server_pid):
    server_inodes = set(socket_inodes(server_pid))
    sockets = [diagnose(inode) for inode in set(socket_inodes(pid))]
    # A client can duplicate its connection and own unrelated local wakeup
    # socketpairs. Match endpoints by process ownership, not mux fd numbers.
    connected = [s for s in sockets if s.get('peer') in server_inodes]
    assert len(connected) == 1, f'expected one client transport, found {connected}'
    receiver = connected[0]
    sender = diagnose(receiver['peer'])
    assert sender['peer'] == receiver['inode'], 'transport peer changed'
    return {'receiver': receiver, 'sender': sender,
            'saturated': sender['send_memory'] >= sender['send_buffer']}
