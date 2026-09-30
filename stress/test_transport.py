"""Validate the independent diagnostic against kernel EAGAIN, without mux."""
import os
import socket
import unittest
from transport import diagnose


class TransportOracle(unittest.TestCase):
    def test_saturation_and_drain_against_socketpair(self):
        left, right = socket.socketpair()
        try:
            inode = int(os.readlink(f'/proc/self/fd/{left.fileno()}')[8:-1])
            self.assertEqual(diagnose(inode)['send_memory'], 0)
            left.setblocking(False)
            delivered = 0
            while True:
                try:
                    delivered += left.send(b'x'*8192)
                except BlockingIOError:
                    break
            full = diagnose(inode)
            peer = diagnose(full['peer'])
            self.assertGreaterEqual(full['send_memory'], full['send_buffer'])
            self.assertEqual(peer['receive_bytes'], delivered)
            received = b''
            while len(received) < delivered:
                received += right.recv(65536)
            empty = diagnose(inode)
            self.assertEqual(empty['send_memory'], 0)
            self.assertEqual(diagnose(full['peer'])['receive_bytes'], 0)
        finally:
            left.close()
            right.close()
