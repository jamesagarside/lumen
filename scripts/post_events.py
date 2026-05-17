#!/usr/bin/env python3
"""Push realistic synthetic detection events to a Lumen daemon.

Pairs with `scripts/send_netflow.py` — together they animate the
demo from nothing: flows populate the graph, detections light it up.

Usage:
    python3 scripts/post_events.py                       # ~5 events, one-shot
    python3 scripts/post_events.py --count 20            # 20 events, one-shot
    python3 scripts/post_events.py --stream              # ~1 every 4s forever
    python3 scripts/post_events.py --stream --rate 0.5   # one every 2s

Auth: by default uses the bootstrap admin (`admin@lumen.test` /
`test1234!`) that `make dev` documents. Override with --email /
--password.
"""
import argparse
import json
import random
import sys
import time
import urllib.error
import urllib.request

# Plausible homelab + UniFi IPs. Roughly matches the flow generator
# so the same nodes show up in both panes.
INTERNAL = [
    "192.168.1.10",   "192.168.1.42",  "192.168.1.50",  "192.168.1.100",
    "192.168.9.245",  "192.168.9.146", "192.168.101.64", "192.168.101.134",
    "192.168.101.207", "10.80.0.11",   "10.80.0.12",
]

EXTERNAL_BAD = [
    "45.33.32.156",   # well-known scanner subnet
    "185.220.101.1",  # Tor exit
    "1.2.3.4",        # placeholder for malicious dest
    "92.255.85.135",  # known C2 in threat lists
    "146.70.92.10",   # vpngate sometimes shows up
]

EXTERNAL_GOOD = [
    "8.8.8.8", "1.1.1.1", "23.246.30.226",   # google dns, cloudflare dns, netflix
    "140.82.121.4",                          # github
    "162.159.135.232",                        # cloudflare / discord
]

# (severity, agent, message_template, rule_id, rule_name, category,
#  direction)
#   direction: 'in'  = external_bad → internal
#              'out' = internal → external_bad
#              'lan' = internal → internal
EVENT_TEMPLATES = [
    (7, "suricata", "ET MALWARE Possible C2 callback (Cobalt Strike) — {dst}", "2031234", "ET MALWARE Cobalt Strike", "intrusion_detection", "out"),
    (7, "unifi-ips", "EXPLOIT remote code execution attempt in HTTP request", "1:39007", "EXPLOIT HTTP RCE", "intrusion_detection", "in"),
    (6, "suricata", "ET SCAN NMAP -sS window 1024 (host scan from {src})", "2008438", "ET SCAN NMAP -sS", "intrusion_detection", "in"),
    (6, "unifi-ips", "BRUTE FORCE login attempts against SSH from {src}", "1:23445", "BRUTE FORCE SSH", "intrusion_detection", "in"),
    (5, "suricata", "ET POLICY DNS query to dynamic DNS provider", "2010493", "ET POLICY Dynamic DNS", "policy_violation", "out"),
    (5, "unifi-ips", "POLICY external SMB connection attempt", "1:11290", "POLICY External SMB", "policy_violation", "out"),
    (4, "suricata", "ET INFO unusual user-agent string from {src}", "2014591", "ET INFO Unusual UA", "anomaly", "out"),
    (3, "synthetic", "Possibly suspicious DNS query (newly registered domain)", None, None, "anomaly", "out"),
    (3, "synthetic", "Plaintext credential candidate observed in flow", None, None, "credential", "out"),
    (2, "synthetic", "New device joined the network: {src}", None, None, "asset_management", "lan"),
]


def now_secs():
    return int(time.time())


def make_event():
    sev, agent, msg_tpl, rule_id, rule_name, category, direction = random.choice(EVENT_TEMPLATES)

    if direction == "in":
        src, dst = random.choice(EXTERNAL_BAD), random.choice(INTERNAL)
    elif direction == "out":
        src = random.choice(INTERNAL)
        dst = random.choice(EXTERNAL_BAD if sev >= 5 else EXTERNAL_GOOD)
    else:
        src = random.choice(INTERNAL)
        dst = random.choice([ip for ip in INTERNAL if ip != src])

    message = msg_tpl.format(src=src, dst=dst)
    event = {
        "@timestamp": {"secs_since_epoch": now_secs(), "nanos_since_epoch": 0},
        "event.kind": "alert",
        "event.category": [category],
        "event.severity": sev,
        "event.action": random.choice(["blocked", "allowed", "logged"]),
        "message": message,
        "agent": {"type": agent, "vendor": "demo"},
        "source.ip": src,
        "destination.ip": dst,
    }
    if rule_id or rule_name:
        rule = {}
        if rule_id:
            rule["id"] = rule_id
        if rule_name:
            rule["name"] = rule_name
        event["rule"] = rule
    if random.random() < 0.4:
        event["url.original"] = f"https://192.168.0.1/protect/alerts/{random.randint(1000, 99999)}"
    return event


def login(base_url, email, password):
    req = urllib.request.Request(
        f"{base_url}/auth/login",
        data=json.dumps({"email": email, "password": password}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        # urlopen doesn't expose Set-Cookie nicely; pull from headers
        cookie = None
        for k, v in resp.getheaders():
            if k.lower() == "set-cookie" and "lumen_session=" in v:
                # take just the name=value, drop attributes
                cookie = v.split(";", 1)[0]
                break
        if not cookie:
            raise RuntimeError("login succeeded but no session cookie returned")
        return cookie


def post_events(base_url, cookie, events):
    body = json.dumps(events).encode()
    req = urllib.request.Request(
        f"{base_url}/ingest/events",
        data=body,
        headers={"Content-Type": "application/json", "Cookie": cookie},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default="http://127.0.0.1:3000", help="daemon base URL")
    parser.add_argument("--email", default="admin@lumen.test")
    parser.add_argument("--password", default="test1234!")
    parser.add_argument("--count", type=int, default=5, help="events in one-shot mode")
    parser.add_argument("--stream", action="store_true", help="keep emitting forever")
    parser.add_argument("--rate", type=float, default=0.25, help="events/sec in stream mode")
    args = parser.parse_args()

    try:
        cookie = login(args.base, args.email, args.password)
    except urllib.error.HTTPError as e:
        sys.stderr.write(f"login failed ({e.code}): {e.reason}\n")
        sys.stderr.write(f"  is the daemon at {args.base} running with LUMEN_INITIAL_ADMIN_EMAIL set?\n")
        sys.exit(1)
    print(f"logged in as {args.email}")

    if args.stream:
        interval = 1.0 / max(args.rate, 0.01)
        sent = 0
        try:
            while True:
                resp = post_events(args.base, cookie, [make_event()])
                sent += resp.get("accepted", 0)
                if sent % 10 == 0:
                    print(f"  streamed {sent} events")
                time.sleep(interval)
        except KeyboardInterrupt:
            print(f"\nstopped after {sent} events")
    else:
        events = [make_event() for _ in range(args.count)]
        resp = post_events(args.base, cookie, events)
        print(f"posted {resp.get('accepted', 0)} events to {args.base}")


if __name__ == "__main__":
    main()
