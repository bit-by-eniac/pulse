#!/usr/bin/env python3
"""A slow network for testing: an HTTP proxy that shapes every connection through it
as one shared link of a given speed and latency.

    scripts/netsim.py --port 8899 --rate-file rate.txt --log link.csv

Point clients at it with `http_proxy=http://127.0.0.1:8899` (mpv, ffmpeg) or
`--proxy` (yt-dlp). HTTPS goes through CONNECT, so nothing is decrypted.
`rate.txt` holds the link speed in bits per second and is re-read every 100 ms
(0 = outage). `examples/network.rs` drives it.
"""

import argparse
import asyncio
import os
import socket
import sys
import time


class Link:
    """A token bucket shared by all connections in one direction. Waiters are served
    in turn (asyncio.Lock is FIFO), which is roughly how a bottleneck queue shares out."""

    def __init__(self, rate_bps):
        self.rate = rate_bps / 8.0
        self.tokens = 0.0
        self.last = time.monotonic()
        self.lock = asyncio.Lock()
        self.bytes = 0

    def burst(self):
        # About 20 ms of traffic, at least one packet.
        return max(1500.0, self.rate * 0.02)

    def chunk(self):
        return int(min(65536, max(512, self.rate * 0.02)))

    async def take(self, n):
        async with self.lock:
            while True:
                now = time.monotonic()
                self.tokens = min(self.burst(), self.tokens + (now - self.last) * self.rate)
                self.last = now
                if self.tokens >= n or (self.rate > 0 and self.tokens >= self.burst() * 0.99):
                    self.tokens -= n
                    self.bytes += n
                    return
                if self.rate <= 0:
                    await asyncio.sleep(0.02)
                    continue
                await asyncio.sleep(max(0.001, (min(n, self.burst()) - self.tokens) / self.rate))


class Net:
    def __init__(self, rate_bps, rtt_ms):
        self.down = Link(rate_bps)
        self.up = Link(rate_bps)
        self.delay = rtt_ms / 2000.0
        self.conns = 0
        self.started = time.monotonic()
        self.conn_log = None

    def set_rate(self, bps):
        for link in (self.down, self.up):
            if link.rate != bps / 8.0:
                now = time.monotonic()
                link.tokens = min(link.burst(), link.tokens + (now - link.last) * link.rate)
                link.last = now
                link.rate = bps / 8.0


async def pump(reader, writer, link, delay, count):
    """Copies one direction through the link, with a fixed one-way delay.
    `count[0]` adds up the bytes."""
    queue = asyncio.Queue()
    queued = [0]

    async def deliver():
        while True:
            at, data = await queue.get()
            if data is None:
                break
            wait = at - time.monotonic()
            if wait > 0:
                await asyncio.sleep(wait)
            writer.write(data)
            await writer.drain()
            queued[0] -= len(data)

    sender = asyncio.ensure_future(deliver())
    try:
        while not sender.done():
            data = await reader.read(link.chunk())
            if not data:
                break
            await link.take(len(data))
            count[0] += len(data)
            await queue.put((time.monotonic() + delay, data))
            queued[0] += len(data)
            # Hold little more than what is on its way along the line: a receiver that
            # stops reading must stop the sender within its own window, as on a real
            # line, not after megabytes queued here have used up the link.
            while queued[0] > max(32768, link.rate * (delay + 0.02)) and not sender.done():
                await asyncio.sleep(0.005)
        if not sender.done():
            await queue.put((0, None))
            await sender
    except (ConnectionError, OSError):
        pass
    finally:
        if not sender.done():
            sender.cancel()
        try:
            writer.write_eof()
        except (OSError, RuntimeError):
            pass


async def handle(net, client_r, client_w):
    net.conns += 1
    upstream_w = None
    opened, host, down, up = time.monotonic(), "?", [0], [0]
    try:
        head = await client_r.readuntil(b"\r\n\r\n")
        line = head.split(b"\r\n", 1)[0].decode("latin-1")
        method, target, _ = line.split(" ", 2)
        if method == "CONNECT":
            host, port = target.rsplit(":", 1)
            rest = b""
        else:
            # Plain HTTP: forward the request with an origin-form path.
            if not target.startswith("http://"):
                client_w.close()
                return
            hostport, _, path = target[len("http://"):].partition("/")
            host, _, port = hostport.partition(":")
            port = port or "80"
            rest = head.replace(target.encode(), b"/" + path.encode(), 1)
        # Opening a connection costs a round trip over the slow link.
        await asyncio.sleep(net.delay * 2)
        upstream_r, upstream_w = await asyncio.open_connection(host, int(port))
        sock = upstream_w.get_extra_info("socket")
        if sock is not None:
            # Keep the kernel from hoarding much more than the link would hold.
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 256 * 1024)
        # What has passed the link waits for the client in small buffers only (the
        # local connection's would otherwise grow to megabytes).
        client_w.transport.set_write_buffer_limits(high=16 * 1024)
        sock = client_w.get_extra_info("socket")
        if sock is not None:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 32 * 1024)
        if method == "CONNECT":
            client_w.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
            await client_w.drain()
        else:
            upstream_w.write(rest)
            await upstream_w.drain()
        # When either side hangs up, the other direction stops too (as the server
        # would on a real line once the client's socket is gone).
        pumps = [
            asyncio.ensure_future(pump(client_r, upstream_w, net.up, net.delay, up)),
            asyncio.ensure_future(pump(upstream_r, client_w, net.down, net.delay, down)),
        ]
        await asyncio.wait(pumps, return_when=asyncio.FIRST_COMPLETED)
        for task in pumps:
            task.cancel()
        await asyncio.gather(*pumps, return_exceptions=True)
    except (ConnectionError, OSError, asyncio.IncompleteReadError, ValueError):
        pass
    finally:
        net.conns -= 1
        if net.conn_log:
            net.conn_log.write(f"{opened - net.started:.1f},{time.monotonic() - net.started:.1f},{host},{down[0]},{up[0]}\n")
            net.conn_log.flush()
        for w in (client_w, upstream_w):
            if w is not None:
                try:
                    w.close()
                except Exception:
                    pass


async def control(net, path, log):
    started = time.monotonic()
    last_log = started
    last = (0, 0)
    current = None
    while True:
        try:
            with open(path) as f:
                bps = float(f.read().strip() or "0")
            if bps != current:
                current = bps
                net.set_rate(bps)
        except (OSError, ValueError):
            pass
        now = time.monotonic()
        if log and now - last_log >= 1.0:
            down, up = net.down.bytes, net.up.bytes
            log.write(f"{now - started:.1f},{current or 0:.0f},{(down - last[0]) * 8 / (now - last_log):.0f},{(up - last[1]) * 8 / (now - last_log):.0f},{net.conns}\n")
            log.flush()
            last, last_log = (down, up), now
        await asyncio.sleep(0.1)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8899)
    ap.add_argument("--rate-file", required=True)
    ap.add_argument("--rtt-ms", type=float, default=60.0)
    ap.add_argument("--log")
    ap.add_argument("--conn-log", help="one line per connection: opened,closed,host,down,up")
    args = ap.parse_args()
    with open(args.rate_file) as f:
        rate = float(f.read().strip() or "0")
    net = Net(rate, args.rtt_ms)
    log = open(args.log, "w") if args.log else None
    if args.conn_log:
        net.conn_log = open(args.conn_log, "w")
        net.conn_log.write("opened,closed,host,down_bytes,up_bytes\n")
    if log:
        log.write("t,limit_bps,down_bps,up_bps,conns\n")
    server = await asyncio.start_server(lambda r, w: handle(net, r, w), "127.0.0.1", args.port)
    print(f"netsim on 127.0.0.1:{args.port}", flush=True)
    asyncio.ensure_future(control(net, args.rate_file, log))
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)
