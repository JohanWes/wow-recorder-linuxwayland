# WR-000: fixture and golden contract

## Existing authoritative raw evidence

The repository already contains real detector logs and end-to-end expected filenames under `tests/logs/` and `tests/src/`. They cover Retail raid wipe/reset/unknown encounter/boss HP, Mythic+ completion/abandon/forced abandon/repair/activity handoff, 2v2/3v3/skirmish/shuffle/BG; Classic raid, 2v2/3v3/5v5/BG and MoP challenge mode; Era raid. These source logs contain player identifiers and are therefore not copied into `tests/native/fixtures/legacy` without a reviewed anonymization pass.

## Minimal retained state-machine paths

| Path | Existing raw fixture / expected behavior | Native anonymized fixture/golden required | Why distinct |
|---|---|---|---|
| Retail raid wipe/kill/reset | `tests/logs/retail/raid_wipe.txt`, `raid_reset.txt`; `tests/src/retail/raid_wipe.py` | one finish fixture with result variants plus one discard/reset | raid duration threshold, boss %, missing-player discard |
| Retail M+ completed | `mythic_plus_repair.txt` → Dawn +18 (+3) | completed dungeon golden | CM duration, upgrade, boss/trash spans |
| Retail M+ abandoned timeout/force/handoff | `mythic_plus.txt`, `mythic_plus_no_boss.txt`, `mythic_plus_ditch_into_raid.txt` | one force/timeout fixture plus handoff sequence | partial combatant, Abandoned, force-end then raid |
| Retail arenas | `rated_2v2.txt`, `rated_3v3.txt`, `skirmish.txt`, `wargame_3v3.txt`, AFK zone-out | shared arena fixture parameterized only by event category; separate zone-out termination | explicit ARENA_MATCH_END versus abandonment |
| Retail Solo Shuffle | `rated_solo_shuffle.txt` | six-round golden | round spans/score and first player death per round |
| Retail battleground | `rated_battleground.txt` | BG zone-in/out golden | death-count estimated result |
| Classic arena 2/3/5 | corresponding `tests/logs/classic/rated_*.txt` | one combatant-count fixture with 2/3/5 snapshots and force-stop variant | category inferred from roster size; 5v5 is reachable |
| Classic battleground | `tests/logs/classic/battleground.txt` | Classic BG golden | Classic zone detector/combatant inference |
| Classic raid | `tests/logs/classic/raid.txt` | Classic raid golden | Classic event shape/difficulty |
| Classic MoP challenge | `tests/logs/classic/mop_challenge_mode.txt` | Classic CM golden | distinct Classic CM handler/timers |
| Era raid | `tests/logs/era/raid.txt` → Thermaplugg kill | Era raid golden | Era handler with Classic flavour metadata |
| Manual | source-driven, no combat log | sidecar/manual command golden | command-driven lifecycle |

The golden schema for every detector path must include category/flavour, log start/detector action/stop action timestamps, `detection_delay_seconds`, configured extra lead-in, calculated replay seconds, title, result including `Abandoned`, activity start/duration/hash, category details, ordered death/timeline points/spans, displayed combatants, and keep/discard with reason.

## Sidecar coverage still required

A minimal set can combine requirements: (1) protected/tagged Raid POV A plus correlated POV B with missing optional fields; (2) abandoned M+; (3) arena; (4) BG; (5) shuffle; (6) Classic raid; (7) Era raid; (8) Classic challenge; (9) Manual; (10) clip parented to one of the preceding entries. Do not create one file per orthogonal flag.

## Evidence status and deferred acceptance

- No `node_modules` or built app is present, so the current parser could not be run to produce trustworthy exact metadata/hash/timestamp goldens.
- The existing logs are not anonymized and include player/GUID data. Inventing simplified log lines or calculated MD5s would create false authoritative evidence. Therefore no fixture/golden files were added in this run.
- The maintainer accepts the source-traced inventory as WR-000's baseline and explicitly defers unavailable artifacts: WR-004 owns smallest consistently anonymized parser fixtures and event goldens; WR-005 owns activity action/metadata/timeline goldens; WR-007 owns representative legacy sidecars and clip/kill media-job goldens; WR-008 owns the headless end-to-end flow evidence. Those tickets must retain the mapping audit back to the existing logs without committing personal identifiers. This report does not claim the current parser was executed or that goldens already exist.
- WR-004 reconciliation dated 2026-07-19: “fixture coverage” means a mapping from every legacy fixture behavior group above to the smallest anonymized distinct parser-shape/path goldens, not copying or replaying every personal raw log. Duplicate raw logs that differ only in activity-state outcomes remain WR-005 evidence. Combat-log timestamps are local civil time; because this report recorded no reliable timezone rule, WR-004 requires the caller to supply and refresh an explicit local year/UTC-offset context rather than deriving UTC from file metadata.

## Skipped (YAGNI)

- No combinatorial codec, incidental roster-size, date, or malformed-log matrix; those do not represent distinct retained state-machine paths.

## Approval

- Approver: maintainer (via user authorization in this session).
- Date/result: 2026-07-19 — source-traced fixture plan approved with artifacts deferred to WR-004/005/007/008; WR-000 `DONE`.
