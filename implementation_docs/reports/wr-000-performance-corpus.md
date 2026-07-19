# WR-000: deterministic performance corpus and reference views

## Corpus identity and filesystem contract

- Generator contract version: `wr-perf-corpus-1`.
- Output contains exactly 2,000 JSON sidecars and 2,000 zero-byte media placeholders; library-load measurements do not decode media.
- Filenames are `wr-perf-0000.mp4` through `wr-perf-1999.mp4` and the same stems with `.json`, using four lowercase decimal digits.
- Base instant is Unix epoch milliseconds `1735689600000` (`2025-01-01T00:00:00.000Z`). Non-Raid item `i` starts at `base + i*1800000`. For Raid ordinal `q=floor(i/10)`, start is `base + floor(q/2)*3600000 + (q mod 2)*5000`; this creates 100 exact two-POV pairs.
- Set both atime and mtime of each `.mp4` and `.json` to the integer Unix second `floor(start/1000) + (d[31] mod 600)`, with zero fractional nanoseconds. File creation order has no semantic effect.
- `manifest.sha256` contains every `.json` and `.mp4`, excludes itself, sorts the 4,000 filenames by their ASCII bytes, and writes lowercase SHA-256, two ASCII spaces, filename, LF. The empty-media digest is therefore repeated. Record the generator source SHA-256 separately in the WR-015 measurement report.

## PRNG and fixed draw order

Seed is unsigned 64-bit `0x57523030305f3230`. Arithmetic below wraps modulo `2^64`; `>>` is an unsigned right shift. Each record initializes its own state to `(seed+i) mod 2^64`, then calls `next()` exactly 32 times and stores the results as `d[0]..d[31]`. Builders never request another random value, including when a field is omitted.

```text
next():
  state = state + 0x9E3779B97F4A7C15
  z = state
  z = (z xor (z >> 30)) * 0xBF58476D1CE4E5B9
  z = (z xor (z >> 27)) * 0x94D049BB133111EB
  return z xor (z >> 31)
```

| Draw | Meaning |
|---|---|
| `d[0]` | duration |
| `d[1]` | result |
| `d[2]` | flavour |
| `d[3]` | primary combatant name |
| `d[4]` | primary spec |
| `d[5]` | primary realm |
| `d[6]` | combatant count |
| `d[7]` | additional-combatant sequence base |
| `d[8]` | protected value (presence is index-driven) |
| `d[9]` | tag value (presence is index-driven) |
| `d[10]` | zone/table row |
| `d[11]` | difficulty |
| `d[12]` | keystone level |
| `d[13]` | affix table offset |
| `d[14]` | death count |
| `d[15]` | death combatant offset |
| `d[16]` | boss percent |
| `d[17]` | map/table row |
| `d[18]` | upgrade level |
| `d[19]` | clip parent category |
| `d[20]` | team MMR |
| `d[21]` | shuffle wins |
| `d[22]` | timeline boss result pattern |
| `d[23]..d[30]` | reserved; consumed and ignored in version 1 |
| `d[31]` | filesystem timestamp offset |

## Finite tables

Index every table with `draw mod table.length`.

```text
categories = ["Raids", "Mythic+", "2v2", "3v3", "5v5",
              "Skirmish", "Solo Shuffle", "Battlegrounds", "Manual", "Clips"]
flavours = ["Retail", "Retail", "Retail", "Classic"]
spec_ids = [0, 62, 63, 64, 65, 66, 70, 71, 72, 73, 102, 103, 104,
            105, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260,
            261, 262, 263, 264, 265, 266, 267, 268, 269, 270, 577, 581,
            1467, 1468, 1473, 1480]
raid_zones = [[9001, "Raid Alpha"], [9002, "Raid Beta"], [9003, "Raid Gamma"]]
raid_encounters = [[9101, "Encounter Alpha"], [9102, "Encounter Beta"],
                   [9103, "Encounter Gamma"], [9104, "Encounter Delta"]]
difficulties = [[17, "LFR"], [14, "N"], [15, "HC"], [16, "M"]]
dungeons = [[9201, 4001, "Dungeon Alpha"], [9202, 4002, "Dungeon Beta"],
            [9203, 4003, "Dungeon Gamma"], [9204, 4004, "Dungeon Delta"]]
pvp_zones = [[9301, "Arena Alpha"], [9302, "Arena Beta"],
             [9303, "Battleground Alpha"], [9304, "Battleground Beta"]]
affix_ids = [1, 2, 4, 7, 10, 11, 124, 128]
clip_parents = ["Raids", "Mythic+", "2v2", "3v3", "5v5",
                "Skirmish", "Solo Shuffle", "Battlegrounds", "Manual"]
```

Names are exactly `Player-00` through `Player-39`; realms are `Realm-00` through `Realm-07`; region is always `XX`. These are synthetic and cannot be mistaken for collected player data.

## Exact record construction and JSON types

`category=categories[i mod 10]`; each category therefore has exactly 200 records. `duration=30+(d[0] mod 3571)` is an integer number of seconds. `result=(d[1] mod 2)==1`, except Manual and a Clip whose parent is Manual are always `true`. `flavour=flavours[d[2] mod 4]`. `overrun=d[0] mod 16`. `start` and `clippedAt` are integer epoch milliseconds. All IDs, MMR, levels, counts, timestamps, durations, and percentages are JSON integers; results/protection are booleans; names, hashes, regions, flavours, dates, and tags are strings.

Create `1+(d[6] mod 5)` combatants. Combatant `j=0` uses `names[d[3] mod 40]`, `spec_ids[d[4] mod 41]`, and `realms[d[5] mod 8]`; later combatant `j` uses `names[(d[7]+j) mod 40]`, `spec_ids[(d[4]+j) mod 41]`, and `realms[(d[5]+j) mod 8]`. Every combatant object has exactly these keys and types:

```json
{"_GUID":"<string>","_name":"<string>","_realm":"<string>","_region":"XX","_specID":0,"_teamID":0}
```

The GUID is `fixture-player-%04d-%02d` with record index and `j`; `_teamID=j mod 2`; name/realm/spec values come from the formulas above. `player` is a copy of combatant zero. When `i mod 13 == 0`, omit `player` and `appVersion` to exercise permissive legacy reads; no other field is implicitly omitted. Otherwise `appVersion="wr-perf-corpus-1"`. Add `protected=(d[8] mod 2)==1` only when `i mod 11 == 0`; add `tag="review-%02d" % (d[9] mod 20)` only when `i mod 7 == 0`.

Every record first receives `category`, `duration`, `result`, `flavour`, `combatants`, `overrun`, `start`, `uniqueHash`, and optional fields above. Non-Raid `uniqueHash` is `perf-item-%04d`. Raid ordinal `q` uses `perf-pov-%03d` with `floor(q/2)`, so each pair shares it. For an odd Raid ordinal, recompute the 32 draws for record `i-10` and use that array as `p`; for an even ordinal, `p=d`. Both pair members use `p` for result, flavour, duration, overrun, zone, encounter, difficulty, boss percent, death data, and their entire roster. Raid roster count is `max(2, 1+(p[6] mod 5))` and uses the combatant formulas with `p`; GUIDs use the even pair member's record index. The even member's `player` is roster entry zero and the odd member's is roster entry one (unless the index-driven sparse rule omits it). This leaves the paired metadata equal where correlation expects it while giving two distinct local viewpoints.

Category fields are then added as follows:

In the Raid bullet and death construction, read `d` as the effective pair array `p` defined above. Clip parents always use the Clip record's own `d`; they do not participate in Raid pairing.

- Raids: `zoneID`/`zoneName` from `raid_zones[d[10]]`; `encounterID`/`encounterName` from `raid_encounters[d[17]]`; `difficultyID`/`difficulty` from `difficulties[d[11]]`; `bossPercent=d[16] mod 101`; and `deaths` below.
- Mythic+: `zoneID`, `mapID`, and `zoneName` from `dungeons[d[17]]`; `keystoneLevel=2+(d[12] mod 29)`; `affixes` is four consecutive values from the circular `affix_ids` table starting at `d[13]`; `upgradeLevel=d[18] mod 4`; `challengeModeTimeline` and `deaths` below.
- 2v2, 3v3, 5v5, and Skirmish: `zoneID`/`zoneName` from `pvp_zones[d[10] mod 2]`; `teamMMR=1000+(d[20] mod 2001)`; and `deaths` below.
- Solo Shuffle: the same arena selection; `soloShuffleRoundsPlayed=6`; `soloShuffleRoundsWon=d[21] mod 7`; `soloShuffleTimeline` and `deaths` below.
- Battlegrounds: `zoneID`/`zoneName` from `pvp_zones[2+(d[10] mod 2)]`; and `deaths` below.
- Manual: no category-specific field.
- Clips: choose `parentCategory=clip_parents[d[19]]`, build that parent's category-specific fields by the rules above, retain top-level `category="Clips"`, and set `clippedAt=start+duration*1000`. Its `uniqueHash` remains the non-Raid item hash even for a Raid parent.

For categories with deaths, `n=d[14] mod 5`. Death `k=0..n-1` selects combatant `(d[15]+k) mod combatant_count`, sets `timestamp=floor((k+1)*duration/(n+1))`, `date` to the ISO-8601 UTC millisecond string for `start+timestamp*1000`, `name`/`specId` from that combatant, and `friendly=(k mod 2)==0`. Preserve increasing `k` order.

Mythic+ has exactly four timeline segments. Segment `k=0..3` starts at `floor(k*duration/4)` seconds and ends at `floor((k+1)*duration/4)` seconds. Even segments are `Trash`; odd segments are `Boss` and add `encounterId=9400+k`. Each object has `segmentType`, ISO UTC `logStart`, ISO UTC `logEnd`, and integer `timestamp` equal to its start offset; do not add `result`, matching the current raw writer.

Solo Shuffle has exactly six ordered objects. Round `r=0..5` is `{round:r+1, timestamp:a, result:((d[22] >> r) & 1)==1, duration:b-a}`, where `a=floor(r*duration/6)` and `b=floor((r+1)*duration/6)`.

## Serialization pseudocode

This pseudocode is normative; a WR-015 implementation in any language must produce the same bytes.

```text
for i in 0..1999:
  d = splitmix_draws(seed + i, 32)
  start = compute_start(i)
  record = build_common_then_category(i, d, start)
  json_bytes = utf8(json_stringify(record,
                                   recursively_sort_object_keys_by_utf8_bytes=true,
                                   array_order_unchanged=true,
                                   indent="  ",
                                   ascii_escape=false) + "\n")
  write wr-perf-%04d.json as json_bytes
  write wr-perf-%04d.mp4 as zero bytes
  set both file timestamps exactly as specified above
write manifest.sha256 from final artifact bytes and sorted artifact names
```

JSON numbers are emitted in base-10 without fractional parts or exponent notation. Strings use standard JSON escaping and literal UTF-8 for non-ASCII (the version-1 tables contain only ASCII). Recursive key sorting is by the unsigned UTF-8 byte sequence, not locale. The final LF is mandatory.

## Reference views required at 1440×900

Current-app screenshots must cover: each of the ten category rows with representative data; setup-required; empty category; filtered-empty with chips/date; multiselect action state; settings sections; active automatic recording/Force end; Manual active; finalizing/error; single-POV player with markers/drawing; multi-POV selector/playback; clip range; kill-video editor/progress; update dialog/progress (migration reference only). Capture dark/light only where current app supports them; do not fabricate a theme it lacks.

## Evidence status

No built/runnable app or populated anonymized library was present, so no reference screenshot is recorded. This acceptance criterion remains blocked on a human-controlled run; screenshot paths and visual pass results are intentionally absent.

## Skipped (YAGNI)

- No Electron baseline benchmark or thumbnail corpus: release gates are absolute and the product has no thumbnail cache.
