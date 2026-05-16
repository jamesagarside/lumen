#!/usr/bin/env python3
"""Send realistic synthetic NetFlow v5 packets to a Lumen daemon.

Usage:
    python3 scripts/send_netflow.py                     # 1 packet, 3-12 random flows
    python3 scripts/send_netflow.py 2055 10             # 10 packets, ~10/sec
    python3 scripts/send_netflow.py 2055 stream         # continuous, ~5 pkt/sec
    python3 scripts/send_netflow.py 2055 stream 20      # continuous, 20 pkt/sec

Each packet contains 3-12 randomly-generated flow records.

Endpoints simulate a typical homelab: a router, a couple of laptops,
phones, an Apple TV, Sonos speakers, a NAS, a few smart-home devices.
External destinations are real IPs of common services (Google,
Cloudflare, GitHub, Apple, Netflix, Discord, etc.) so ASN clustering
in the UI looks plausible.

Protocol distribution is weighted realistically: HTTPS dominates,
DNS is frequent, with a long tail of mDNS, SSDP, NTP, Plex, etc.
Byte sizes follow a long-tailed distribution: most flows small,
occasional bursts.
"""
import random
import socket
import struct
import sys
import time


# Plausible homelab inventory.
INTERNAL = [
    "192.168.1.1",     # gateway
    "192.168.1.10",    # macbook
    "192.168.1.11",    # iphone
    "192.168.1.12",    # android-phone
    "192.168.1.20",    # apple-tv
    "192.168.1.21",    # sonos-livingroom
    "192.168.1.22",    # sonos-kitchen
    "192.168.1.30",    # nas
    "192.168.1.40",    # homepod
    "192.168.1.50",    # smart-tv
    "192.168.1.60",    # thermostat
    "192.168.1.61",    # doorbell
    "192.168.1.62",    # camera-frontdoor
    "192.168.1.100",   # desktop-windows
    "192.168.1.200",   # dev-laptop
]

# Real IPs of common services. Multiple per service so ASN/brand
# clustering has something to work with later.
EXTERNAL_SERVICES = [
    ("google",     ["8.8.8.8", "8.8.4.4", "142.250.179.142", "172.217.20.46"]),
    ("cloudflare", ["1.1.1.1", "1.0.0.1", "104.16.132.229", "162.159.135.232"]),
    ("github",     ["140.82.121.4", "140.82.114.6", "185.199.108.153"]),
    ("apple",      ["17.253.5.55", "17.253.4.51", "17.57.146.20"]),
    ("netflix",    ["23.246.30.226", "23.246.40.226", "108.175.32.50"]),
    ("spotify",    ["35.186.224.25", "104.199.65.124"]),
    ("discord",    ["162.159.137.232", "162.159.135.234"]),
    ("reddit",     ["199.232.214.108", "151.101.1.140"]),
    ("microsoft",  ["13.107.42.14", "52.184.40.83"]),
    ("aws",        ["52.84.150.39", "52.94.236.248"]),
    ("twitch",     ["151.101.66.167", "23.160.0.155"]),
    ("youtube",    ["172.217.16.110", "216.58.205.78"]),
]

# (dst_port, proto, byte_min, byte_max, weight)
# weights bias towards realistic frequencies on a typical home network.
PROTOCOLS = [
    (443,   6,    100,  500_000, 70),  # https — dominates
    (80,    6,    100,    5_000, 4),   # http
    (53,    17,    50,      200, 12),  # dns
    (5353,  17,   100,      500, 3),   # mdns / bonjour
    (1900,  17,   100,      500, 2),   # ssdp
    (123,   17,    50,      100, 1),   # ntp
    (32400, 6,  1_000,   50_000, 2),   # plex
    (1883,  6,     50,      500, 1),   # mqtt
    (22,    6,    100,    2_000, 1),   # ssh
    (3478,  17,   100,    2_000, 2),   # webrtc / stun
    (1935,  6,  1_000,   20_000, 1),   # rtmp
    (51820, 17,   500,    5_000, 1),   # wireguard
]
PROTO_WEIGHTS = [p[4] for p in PROTOCOLS]


def random_external() -> str:
    _, ips = random.choice(EXTERNAL_SERVICES)
    return random.choice(ips)


def random_internal() -> str:
    return random.choice(INTERNAL)


def long_tail_bytes(min_b: int, max_b: int) -> int:
    """Most samples cluster near min, occasional ones approach max."""
    span = max_b - min_b
    # 10% chance of a "large" flow.
    if random.random() < 0.1:
        return min_b + int(span * random.random())
    # Triangular weighted to the small end otherwise.
    return min_b + int(span * (random.random() ** 3))


def random_flow():
    """Return (src, dst, src_port, dst_port, proto, bytes, packets)."""
    dst_port, proto, bmin, bmax, _ = random.choices(
        PROTOCOLS, weights=PROTO_WEIGHTS, k=1
    )[0]
    bytes_count = long_tail_bytes(bmin, bmax)
    # Average ~1KB/packet for TCP, larger for video; keep simple here.
    packets = max(1, bytes_count // random.randint(500, 1500))

    if proto == 17 and dst_port in (5353, 1900):
        # Multicast-style: internal -> internal
        src = random_internal()
        dst = random_internal()
        # Don't loop a host to itself; if it happens just retry once.
        if src == dst:
            dst = random_internal()
        return (src, dst, random.randint(1024, 65535),
                dst_port, proto, bytes_count, packets)
    elif random.random() < 0.85:
        # Outbound
        return (random_internal(), random_external(),
                random.randint(1024, 65535), dst_port, proto,
                bytes_count, packets)
    else:
        # Inbound (response side of a flow)
        return (random_external(), random_internal(), dst_port,
                random.randint(1024, 65535), proto, bytes_count, packets)


def build_packet(records) -> bytes:
    header = struct.pack(
        ">HHIIIIBBH",
        5,
        len(records),
        random.randint(60_000, 86_400_000),  # sys_uptime ms
        int(time.time()),
        0,
        random.randint(0, 2_000_000_000),    # flow_sequence
        0, 0, 0,
    )
    body = b""
    for src, dst, sport, dport, proto, bytes_, packets in records:
        body += struct.pack(
            ">4s4s4sHHIIIIHHBBBBHHBBH",
            socket.inet_aton(src),
            socket.inet_aton(dst),
            b"\x00\x00\x00\x00",
            0, 0,
            packets, bytes_,
            random.randint(0, 60_000),       # first uptime ms
            random.randint(60_001, 120_000), # last uptime ms
            sport, dport,
            0, 0, proto, 0,
            0, 0, 0, 0, 0,
        )
    return header + body


def send_packet(sock: socket.socket, port: int) -> int:
    n_records = random.randint(3, 12)
    records = [random_flow() for _ in range(n_records)]
    pkt = build_packet(records)
    sock.sendto(pkt, ("127.0.0.1", port))
    return n_records


def main() -> None:
    args = sys.argv[1:]
    port = int(args[0]) if args else 2055
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

    # Stream mode: keep going until interrupted.
    if len(args) > 1 and args[1] == "stream":
        rate = int(args[2]) if len(args) > 2 else 5
        delay = 1.0 / rate
        print(f"streaming ~{rate} packets/sec to 127.0.0.1:{port} (Ctrl-C to stop)")
        sent_pkts = 0
        sent_records = 0
        try:
            while True:
                sent_records += send_packet(sock, port)
                sent_pkts += 1
                if sent_pkts % 50 == 0:
                    print(f"  sent {sent_pkts} packets / {sent_records} records")
                time.sleep(delay)
        except KeyboardInterrupt:
            print(f"\nstopped after {sent_pkts} packets / {sent_records} records")
        return

    count = int(args[1]) if len(args) > 1 else 1
    sent_records = 0
    for _ in range(count):
        sent_records += send_packet(sock, port)
        if count > 1:
            time.sleep(0.1)
    print(f"sent {count} packet(s), {sent_records} flow records to 127.0.0.1:{port}")


if __name__ == "__main__":
    main()
