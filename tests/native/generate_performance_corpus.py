#!/usr/bin/env python3
"""Generate the deterministic WR-000 2,000-sidecar performance corpus."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path


MASK = (1 << 64) - 1
SEED = 0x57523030305F3230
BASE_MS = 1_735_689_600_000
CATEGORIES = [
    "Raids", "Mythic+", "2v2", "3v3", "5v5", "Skirmish",
    "Solo Shuffle", "Battlegrounds", "Manual", "Clips",
]
FLAVOURS = ["Retail", "Retail", "Retail", "Classic"]
SPEC_IDS = [
    0, 62, 63, 64, 65, 66, 70, 71, 72, 73, 102, 103, 104, 105, 250,
    251, 252, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263,
    264, 265, 266, 267, 268, 269, 270, 577, 581, 1467, 1468, 1473, 1480,
]
RAID_ZONES = [(9001, "Raid Alpha"), (9002, "Raid Beta"), (9003, "Raid Gamma")]
RAID_ENCOUNTERS = [
    (9101, "Encounter Alpha"), (9102, "Encounter Beta"),
    (9103, "Encounter Gamma"), (9104, "Encounter Delta"),
]
DIFFICULTIES = [(17, "LFR"), (14, "N"), (15, "HC"), (16, "M")]
DUNGEONS = [
    (9201, 4001, "Dungeon Alpha"), (9202, 4002, "Dungeon Beta"),
    (9203, 4003, "Dungeon Gamma"), (9204, 4004, "Dungeon Delta"),
]
PVP_ZONES = [
    (9301, "Arena Alpha"), (9302, "Arena Beta"),
    (9303, "Battleground Alpha"), (9304, "Battleground Beta"),
]
AFFIX_IDS = [1, 2, 4, 7, 10, 11, 124, 128]
CLIP_PARENTS = CATEGORIES[:-1]


def draws(index: int) -> list[int]:
    state = (SEED + index) & MASK
    values = []
    for _ in range(32):
        state = (state + 0x9E3779B97F4A7C15) & MASK
        value = state
        value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & MASK
        values.append((value ^ (value >> 31)) & MASK)
    return values


def iso_ms(value: int) -> str:
    instant = dt.datetime.fromtimestamp(value / 1000, tz=dt.timezone.utc)
    return instant.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def start_ms(index: int, category: str) -> int:
    if category != "Raids":
        return BASE_MS + index * 1_800_000
    ordinal = index // 10
    return BASE_MS + (ordinal // 2) * 3_600_000 + (ordinal % 2) * 5_000


def combatants(index: int, values: list[int], count: int, guid_index: int) -> list[dict]:
    result = []
    for item in range(count):
        if item == 0:
            name_index = values[3] % 40
            realm_index = values[5] % 8
        else:
            name_index = (values[7] + item) % 40
            realm_index = (values[5] + item) % 8
        result.append({
            "_GUID": f"fixture-player-{guid_index:04d}-{item:02d}",
            "_name": f"Player-{name_index:02d}",
            "_realm": f"Realm-{realm_index:02d}",
            "_region": "XX",
            "_specID": SPEC_IDS[(values[4] + (item if item else 0)) % len(SPEC_IDS)],
            "_teamID": item % 2,
        })
    return result


def deaths(values: list[int], roster: list[dict], duration: int, start: int) -> list[dict]:
    count = values[14] % 5
    result = []
    for item in range(count):
        player = roster[(values[15] + item) % len(roster)]
        timestamp = ((item + 1) * duration) // (count + 1)
        result.append({
            "timestamp": timestamp,
            "date": iso_ms(start + timestamp * 1000),
            "name": player["_name"],
            "specId": player["_specID"],
            "friendly": item % 2 == 0,
        })
    return result


def add_category_fields(
    record: dict,
    category: str,
    values: list[int],
    roster: list[dict],
    duration: int,
    start: int,
) -> None:
    if category == "Raids":
        record["zoneID"], record["zoneName"] = RAID_ZONES[values[10] % len(RAID_ZONES)]
        record["encounterID"], record["encounterName"] = RAID_ENCOUNTERS[
            values[17] % len(RAID_ENCOUNTERS)
        ]
        record["difficultyID"], record["difficulty"] = DIFFICULTIES[
            values[11] % len(DIFFICULTIES)
        ]
        record["bossPercent"] = values[16] % 101
        record["deaths"] = deaths(values, roster, duration, start)
    elif category == "Mythic+":
        record["zoneID"], record["mapID"], record["zoneName"] = DUNGEONS[
            values[17] % len(DUNGEONS)
        ]
        record["keystoneLevel"] = 2 + values[12] % 29
        offset = values[13] % len(AFFIX_IDS)
        record["affixes"] = [AFFIX_IDS[(offset + item) % len(AFFIX_IDS)] for item in range(4)]
        record["upgradeLevel"] = values[18] % 4
        timeline = []
        for item in range(4):
            begin = item * duration // 4
            end = (item + 1) * duration // 4
            segment = {
                "segmentType": "Trash" if item % 2 == 0 else "Boss",
                "logStart": iso_ms(start + begin * 1000),
                "logEnd": iso_ms(start + end * 1000),
                "timestamp": begin,
            }
            if item % 2:
                segment["encounterId"] = 9400 + item
            timeline.append(segment)
        record["challengeModeTimeline"] = timeline
        record["deaths"] = deaths(values, roster, duration, start)
    elif category in {"2v2", "3v3", "5v5", "Skirmish"}:
        record["zoneID"], record["zoneName"] = PVP_ZONES[values[10] % 2]
        record["teamMMR"] = 1000 + values[20] % 2001
        record["deaths"] = deaths(values, roster, duration, start)
    elif category == "Solo Shuffle":
        record["zoneID"], record["zoneName"] = PVP_ZONES[values[10] % 2]
        record["soloShuffleRoundsPlayed"] = 6
        record["soloShuffleRoundsWon"] = values[21] % 7
        record["soloShuffleTimeline"] = [
            {
                "round": item + 1,
                "timestamp": item * duration // 6,
                "result": ((values[22] >> item) & 1) == 1,
                "duration": (item + 1) * duration // 6 - item * duration // 6,
            }
            for item in range(6)
        ]
        record["deaths"] = deaths(values, roster, duration, start)
    elif category == "Battlegrounds":
        record["zoneID"], record["zoneName"] = PVP_ZONES[2 + values[10] % 2]
        record["deaths"] = deaths(values, roster, duration, start)


def build_record(index: int) -> tuple[dict, int]:
    category = CATEGORIES[index % len(CATEGORIES)]
    own_values = draws(index)
    start = start_ms(index, category)
    values = own_values
    guid_index = index
    if category == "Raids":
        ordinal = index // 10
        if ordinal % 2:
            values = draws(index - 10)
            guid_index = index - 10
        roster_count = max(2, 1 + values[6] % 5)
    else:
        roster_count = 1 + values[6] % 5
    roster = combatants(index, values, roster_count, guid_index)
    duration = 30 + values[0] % 3571
    parent = CLIP_PARENTS[own_values[19] % len(CLIP_PARENTS)] if category == "Clips" else category
    result = values[1] % 2 == 1
    if category == "Manual" or (category == "Clips" and parent == "Manual"):
        result = True
    unique_hash = (
        f"perf-pov-{(index // 10) // 2:03d}"
        if category == "Raids"
        else f"perf-item-{index:04d}"
    )
    record = {
        "category": category,
        "duration": duration,
        "result": result,
        "flavour": FLAVOURS[values[2] % len(FLAVOURS)],
        "combatants": roster,
        "overrun": values[0] % 16,
        "start": start,
        "uniqueHash": unique_hash,
    }
    if index % 13 != 0:
        player_index = 1 if category == "Raids" and (index // 10) % 2 else 0
        record["player"] = dict(roster[player_index])
        record["appVersion"] = "wr-perf-corpus-1"
    if index % 11 == 0:
        record["protected"] = own_values[8] % 2 == 1
    if index % 7 == 0:
        record["tag"] = f"review-{own_values[9] % 20:02d}"
    if category == "Clips":
        record["parentCategory"] = parent
        add_category_fields(record, parent, own_values, roster, duration, start)
        record["clippedAt"] = start + duration * 1000
    else:
        add_category_fields(record, category, values, roster, duration, start)
    return record, start + own_values[31] % 600 * 1000


def generate(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    artifacts = []
    for index in range(2000):
        record, timestamp_ms = build_record(index)
        stem = f"wr-perf-{index:04d}"
        sidecar = output / f"{stem}.json"
        media = output / f"{stem}.mp4"
        sidecar.write_text(
            json.dumps(record, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        media.write_bytes(b"")
        timestamp = timestamp_ms // 1000
        os.utime(sidecar, ns=(timestamp * 1_000_000_000,) * 2)
        os.utime(media, ns=(timestamp * 1_000_000_000,) * 2)
        artifacts.extend((sidecar, media))
    lines = []
    for path in sorted(artifacts, key=lambda item: item.name.encode("ascii")):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {path.name}\n")
    (output / "manifest.sha256").write_text("".join(lines), encoding="ascii")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    generate(args.output)


if __name__ == "__main__":
    main()
