#!/usr/bin/env python3
"""Generates a folder of realistic log4j / Logback logs for testing and benchmarking.

Three services with different layouts, rolled files (app.log, app.log.1, dated .gz files),
multi-line stack traces with Caused-by chains, MDC request ids and a JSON-layout service.

    python3 scripts/gen_log4j_logs.py --out testdata/generated --size-mb 50
"""

import argparse
import gzip
import json
import os
import random
from datetime import datetime, timedelta, timezone

LEVELS = [("INFO", 70), ("DEBUG", 15), ("WARN", 9), ("ERROR", 5), ("TRACE", 1)]
LOGGERS = [
    "com.acme.payment.PaymentService",
    "com.acme.payment.gateway.StripeClient",
    "com.acme.order.OrderController",
    "com.acme.order.OrderRepository",
    "com.acme.auth.TokenFilter",
    "org.springframework.web.servlet.DispatcherServlet",
    "org.hibernate.SQL",
    "com.zaxxer.hikari.pool.HikariPool",
]
EXCEPTIONS = [
    ("java.net.SocketTimeoutException", "Read timed out", "java.io.IOException"),
    ("org.springframework.dao.DataIntegrityViolationException", "could not execute statement", "java.sql.SQLIntegrityConstraintViolationException"),
    ("java.lang.IllegalStateException", "Order already paid", None),
    ("java.lang.NullPointerException", "Cannot invoke \"String.length()\" because \"s\" is null", None),
]
MESSAGES = [
    "Processing payment for order {order} amount={amount} currency=EUR",
    "GET /api/orders/{order} status={status} duration={dur}ms",
    "POST /api/payments status={status} duration={dur}ms",
    "Token validated for user={user}",
    "HikariPool-1 - Pool stats (total=10, active={active}, idle={idle}, waiting=0)",
    "select o.id, o.status from orders o where o.id=?",
    "Cache miss for key order:{order}",
    'Payload {{"orderId":{order},"status":{status},"duration":"{dur}ms","user":"{user}"}}',
]


def pick_level(rng):
    r = rng.randint(1, 100)
    acc = 0
    for name, weight in LEVELS:
        acc += weight
        if r <= acc:
            return name
    return "INFO"


def message(rng):
    return rng.choice(MESSAGES).format(
        order=rng.randint(1000, 99999),
        amount=round(rng.uniform(1, 500), 2),
        status=rng.choice([200, 200, 200, 201, 404, 500, 503]),
        dur=rng.randint(1, 5000),
        user=rng.choice(["alice", "bob", "carol", "dave"]),
        active=rng.randint(0, 10),
        idle=rng.randint(0, 10),
    )


def stack_trace(rng):
    exc, msg, cause = rng.choice(EXCEPTIONS)
    lines = [f"{exc}: {msg}"]
    for _ in range(rng.randint(4, 12)):
        cls = rng.choice(LOGGERS)
        lines.append(f"\tat {cls}.{rng.choice(['handle', 'process', 'call', 'invoke'])}({cls.split('.')[-1]}.java:{rng.randint(20, 400)})")
    if cause:
        lines.append(f"Caused by: {cause}: underlying failure")
        lines.append(f"\tat java.base/java.net.SocketInputStream.read(SocketInputStream.java:{rng.randint(100, 200)})")
        lines.append(f"\t... {rng.randint(5, 40)} more")
    return lines


def text_event(rng, ts, layout):
    level = pick_level(rng)
    logger = rng.choice(LOGGERS)
    thread = rng.choice(["main", "http-nio-8080-exec-1", "http-nio-8080-exec-7", "scheduling-1", "HikariPool-1 housekeeper"])
    req = f"req-{rng.randint(1, 5000):05d}"
    msg = message(rng)
    if layout == "payments":
        # %d{yyyy-MM-dd HH:mm:ss.SSS} [%t] %-5level %logger{36} [%X{requestId}] - %msg%n
        first = f"{ts.strftime('%Y-%m-%d %H:%M:%S')}.{ts.microsecond // 1000:03d} [{thread}] {level:<5} {logger} [{req}] - {msg}"
    else:
        # %d{ISO8601} %-5p [%t] %c{1}:%L - %m%n
        first = f"{ts.strftime('%Y-%m-%dT%H:%M:%S')},{ts.microsecond // 1000:03d} {level:<5} [{thread}] {logger.split('.')[-1]}:{rng.randint(10, 500)} - {msg}"
    lines = [first]
    if level == "ERROR" and rng.random() < 0.7:
        lines += stack_trace(rng)
    return lines


def json_event(rng, ts):
    level = pick_level(rng)
    ev = {
        "@timestamp": ts.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ts.microsecond // 1000:03d}Z",
        "log.level": level,
        "log.logger": rng.choice(LOGGERS),
        "process.thread.name": rng.choice(["main", "worker-1", "worker-2"]),
        "message": message(rng),
        "requestId": f"req-{rng.randint(1, 5000):05d}",
    }
    if level == "ERROR":
        st = stack_trace(rng)
        ev["error.type"] = st[0].split(":")[0]
        ev["error.stack_trace"] = "\n".join(st)
    return [json.dumps(ev)]


def write_service(out, name, layout, total_bytes, start, rng):
    os.makedirs(os.path.join(out, name), exist_ok=True)
    per_file = max(total_bytes // 5, 64 * 1024)
    ts = start
    files = []
    # Oldest first: three dated gz files, then app.log.1, then app.log.
    names = [f"app-{(start + timedelta(days=i)).strftime('%Y-%m-%d')}-1.log.gz" for i in range(3)] + ["app.log.1", "app.log"]
    written = 0
    for fname in names:
        buf = []
        size = 0
        while size < per_file:
            ts += timedelta(milliseconds=rng.randint(1, 400))
            lines = json_event(rng, ts) if layout == "json" else text_event(rng, ts, layout)
            for l in lines:
                buf.append(l)
                size += len(l) + 1
        data = ("\n".join(buf) + "\n").encode()
        path = os.path.join(out, name, fname)
        if fname.endswith(".gz"):
            with gzip.open(path, "wb", compresslevel=1) as f:
                f.write(data)
        else:
            with open(path, "wb") as f:
                f.write(data)
        written += len(data)
        files.append(path)
    return written


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="testdata/generated")
    ap.add_argument("--size-mb", type=float, default=20)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    total = int(args.size_mb * 1024 * 1024)
    start = datetime(2026, 10, 5, 8, 0, 0, tzinfo=timezone.utc)
    written = 0
    written += write_service(args.out, "payments", "payments", total * 5 // 10, start, rng)
    written += write_service(args.out, "orders", "orders", total * 3 // 10, start, rng)
    written += write_service(args.out, "gateway", "json", total * 2 // 10, start, rng)
    print(f"wrote {written / 1e6:.1f} MB (uncompressed) to {args.out}")


if __name__ == "__main__":
    main()
