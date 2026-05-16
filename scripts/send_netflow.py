#!/usr/bin/env python3
"""Send a synthetic NetFlow v5 packet to a Lumen daemon for smoke testing.

Usage:
    python3 scripts/send_netflow.py [port] [count]

Defaults: port=2055, count=1 packet.

Each packet contains 2 flow records:
  - 192.168.1.100:51234 -> 8.8.8.8:53 (UDP, 256B, 4 packets)
  - 192.168.1.100:60001 -> 140.82.121.4:443 (TCP, 12345B, 17 packets)
"""
import socket
import struct
import sys
import time


def build_packet() -> bytes:
    header = struct.pack(
        ">HHIIIIBBH",
        5,                  # version
        2,                  # count
        60_000,             # sys_uptime ms
        int(time.time()),   # unix_secs
        0,                  # unix_nsecs
        0,                  # flow_seq
        0, 0,               # engine_type, engine_id
        0,                  # sampling
    )
    record_a = struct.pack(
        ">4s4s4sHHIIIIHHBBBBHHBBH",
        socket.inet_aton("192.168.1.100"),
        socket.inet_aton("8.8.8.8"),
        b"\x00\x00\x00\x00",
        0, 0,
        4, 256,
        30_000, 55_000,
        51_234, 53,
        0, 0, 17, 0,
        0, 0, 0, 0, 0,
    )
    record_b = struct.pack(
        ">4s4s4sHHIIIIHHBBBBHHBBH",
        socket.inet_aton("192.168.1.100"),
        socket.inet_aton("140.82.121.4"),
        b"\x00\x00\x00\x00",
        0, 0,
        17, 12_345,
        30_000, 55_000,
        60_001, 443,
        0, 0, 6, 0,
        0, 0, 0, 0, 0,
    )
    return header + record_a + record_b


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 2055
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    pkt = build_packet()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    for i in range(count):
        sock.sendto(pkt, ("127.0.0.1", port))
        if count > 1:
            time.sleep(0.1)
    print(f"sent {count} packet(s), {len(pkt)} B each (2 records each) to 127.0.0.1:{port}")


if __name__ == "__main__":
    main()
