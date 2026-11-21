#!/usr/bin/env python3
"""Compute expected CSVs for small Shopping Sessions inputs using the Python stdlib."""

import argparse
import csv
import json
import re
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path

DAY = 86_400_000
WEEK = 7 * DAY
GAP = 1_800_000
EPOCH = datetime(1970, 1, 1)
ISO = re.compile(
    rb"([0-9]{4})-([0-9]{2})-([0-9]{2})T"
    rb"([0-9]{2}):([0-9]{2}):([0-9]{2})"
    rb"(Z|([+-])([0-9]{2}):([0-9]{2}))"
)
USER_COLUMNS = ("user_id", "day", "sessions", "events", "revenue_cents", "top_category")
CAMPAIGN_COLUMNS = (
    "campaign_id", "channel", "day", "sessions",
    "purchasing_sessions", "revenue_cents", "assisted_sessions",
)


def records(files, width):
    for path in files:
        with path.open("rb") as src:
            next(src, None)
            for line in src:
                columns = line.removesuffix(b"\n").split(b",")
                if len(columns) != width:
                    continue
                for index, value in enumerate(columns):
                    if value.startswith(b'"') and value.endswith(b'"') and len(value) >= 2:
                        value = value[1:-1]
                    if b'"' in value:
                        break
                    columns[index] = value
                else:
                    yield columns


def decimal(raw, maximum):
    if not raw.isdigit():
        return None
    digits = raw.lstrip(b"0") or b"0"
    if len(digits) > len(str(maximum)):
        return None
    value = int(digits)
    return value if value <= maximum else None


def timestamp(raw):
    if raw.isdigit():
        return decimal(raw, (1 << 63) - 1)
    match = ISO.fullmatch(raw)
    if not match:
        return None
    try:
        local = datetime(*map(int, match.groups()[:6]))
    except ValueError:
        return None
    wall_time = local - EPOCH
    milliseconds = wall_time.days * DAY + wall_time.seconds * 1000
    if match[7] != b"Z":
        hour, minute = int(match[9]), int(match[10])
        if hour > 23 or minute > 59:
            return None
        offset = (hour * 60 + minute) * 60_000
        milliseconds += offset if match[8] == b"-" else -offset
    return milliseconds


def people(data):
    parents = {}

    def root(user):
        while user in parents and parents[user] != user:
            parents[user] = parents[parents[user]]
            user = parents[user]
        return user

    for a, b in records((data / "links").glob("*.csv"), 2):
        if a and b:
            left, right = sorted((root(a), root(b)))
            parents.setdefault(left, left)
            parents[right] = left
    return root


def load_events(data, window, root, categories):
    earliest = {}
    for row in records((data / "events").glob("*.csv"), 7):
        event_id = row[0]
        if event_id and (event_id not in earliest or row[3] < earliest[event_id][3]):
            earliest[event_id] = row

    by_person = defaultdict(list)
    for _, user, raw_time, ingest, kind, product, raw_price in earliest.values():
        ts = timestamp(raw_time)
        if not user or not product or ts is None or not window[0] <= ts < window[1]:
            continue
        if kind not in (b"purchase", b"cart", b"view") or timestamp(ingest) - ts > DAY:
            continue
        price = 0
        if kind == b"purchase":
            price = decimal(raw_price, 10_000_000)
            if not price:
                continue
        by_person[root(user)].append((ts, price, categories.get(product, b"unknown")))
    return by_person


def load_clicks(data, window, root, campaigns):
    copies = {}
    for click_id, user, campaign, raw_time in records((data / "clicks").glob("*.csv"), 4):
        if click_id:
            copies.setdefault(click_id, (user, campaign, raw_time))

    by_person = defaultdict(list)
    for click_id, (user, campaign, raw_time) in copies.items():
        ts = timestamp(raw_time)
        if user and campaign in campaigns and ts is not None and window[0] - WEEK <= ts < window[1]:
            by_person[root(user)].append((ts, click_id, campaign))
    return by_person


@dataclass
class UserDay:
    sessions: int = 0
    events: int = 0
    revenue: int = 0
    categories: dict = field(default_factory=lambda: defaultdict(int))


@dataclass
class CampaignDay:
    credited: int = 0
    purchasing: int = 0
    revenue: int = 0
    assists: int = 0


def sessions(events):
    events = sorted(events)
    first = 0
    for i in range(1, len(events)):
        if events[i][0] - events[i - 1][0] > GAP:
            yield events[first:i]
            first = i
    if events:
        yield events[first:]


def write_csv(path, columns, rows):
    with path.open("w", encoding="utf-8", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(columns)
        for row in rows:
            writer.writerow(value.decode("utf-8") if isinstance(value, bytes) else value for value in row)


def run(data, output):
    manifest = json.loads((data / "manifest.json").read_text(encoding="utf-8"))
    window = tuple(timestamp(manifest[k].encode()) for k in ("window_start", "window_end"))
    root = people(data)
    products = dict(records([data / "products.csv"], 2))
    channels = dict(records([data / "campaigns.csv"], 2))
    events = load_events(data, window, root, products)
    clicks = load_clicks(data, window, root, channels)
    channels[b"organic"] = b"organic"
    users = defaultdict(UserDay)
    campaigns = defaultdict(CampaignDay)

    for person, rows in events.items():
        for session in sessions(rows):
            start = session[0][0]
            day = EPOCH.date() + timedelta(days=start // DAY)
            eligible = [click for click in clicks[person] if start - WEEK <= click[0] <= start]
            credited = min(eligible, key=lambda click: (-click[0], click[1]))[2] if eligible else b"organic"
            for campaign in {click[2] for click in eligible}:
                campaigns[campaign, day].assists += 1

            user = users[person, day]
            user.sessions += 1
            user.events += len(session)
            revenue = 0
            for _, price, category in session:
                if price:
                    user.categories[category] += price
                    revenue += price
            user.revenue += revenue
            campaign = campaigns[credited, day]
            campaign.credited += 1
            campaign.purchasing += int(revenue > 0)
            campaign.revenue += revenue

    def user_rows():
        for (person, day), user in sorted(users.items()):
            top = min(user.categories, key=lambda c: (-user.categories[c], c), default=None)
            yield person, day, user.sessions, user.events, user.revenue, top

    def campaign_rows():
        for (cid, day), stats in sorted(campaigns.items()):
            yield cid, channels[cid], day, stats.credited, stats.purchasing, stats.revenue, stats.assists

    output.mkdir(parents=True, exist_ok=True)
    write_csv(output / "user_daily.csv", USER_COLUMNS, user_rows())
    write_csv(output / "campaign_daily.csv", CAMPAIGN_COLUMNS, campaign_rows())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("data", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    run(args.data, args.output)
