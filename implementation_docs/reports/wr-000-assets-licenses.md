# WR-000: legacy metadata, media, assets and licenses

## Metadata contract

Legacy sidecars are JSON adjacent to media. Exact permissive fields are defined at `src/main/types.ts:195-249`; all fields of nested combatants may be absent. Activity writers are `RaidEncounter.ts:138-164`, `ChallengeModeDungeon.ts:201-228`, `ArenaMatch.ts:78-97`, `Battleground.ts:63-77`, `SoloShuffle.ts:195-218`, and `Manual.ts:17-28`. Clips copy source metadata, set category Clips, parentCategory and clippedAt (`src/main/util.ts:602-647`). Tag/protect patching mutates only `tag`/`protected` (`src/storage/DiskClient.ts:149-197`).

No real legacy library was available, so no file is represented as a collected user sidecar. Downstream must accept optional/missing old fields exactly as the type and storage reader do; synthetic goldens may test this but cannot replace the required real samples.

## Media evidence

- H.264 with audio: not available; no path/hash invented.
- AV1 with audio: not available; no path/hash invented.
- WR-002/WR-011 remain blocked on representative redistributable or private/manual media paths plus SHA-256.

## Exact asset inventory

The six packaged families contain exactly 125 files: 13 class, 41 spec, 40 affix, 25 icon, 3 role, and 3 sound files. The package asset glob ships all 125, but source references do not prove every packaged file is reachable. Root `assets/icon.png` is a 126th asset outside those six directories and is used by `install.sh` as the installed application icon.

- `assets/class/` (all imported): `deathknight.png`, `demonhunter.png`, `druid.png`, `evoker.png`, `hunter.png`, `mage.png`, `monk.png`, `paladin.png`, `priest.png`, `rogue.png`, `shaman.png`, `warlock.png`, `warrior.png`.
- `assets/specs/` (all imported): `0.png`, `62.png`, `63.png`, `64.png`, `65.png`, `66.png`, `70.png`, `71.png`, `72.png`, `73.png`, `102.png`, `103.png`, `104.png`, `105.png`, `250.png`, `251.png`, `252.png`, `253.png`, `254.png`, `255.png`, `256.png`, `257.png`, `258.png`, `259.png`, `260.png`, `261.png`, `262.png`, `263.png`, `264.png`, `265.png`, `266.png`, `267.png`, `268.png`, `269.png`, `270.png`, `577.png`, `581.png`, `1467.png`, `1468.png`, `1473.png`, `1480.png`.
- `assets/affixes/` (all imported): `1.jpg`, `2.jpg`, `3.jpg`, `4.jpg`, `5.jpg`, `6.jpg`, `7.jpg`, `8.jpg`, `9.jpg`, `10.jpg`, `11.jpg`, `12.jpg`, `13.jpg`, `14.jpg`, `117.jpg`, `120.jpg`, `121.jpg`, `122.jpg`, `123.jpg`, `124.jpg`, `128.jpg`, `130.jpg`, `131.jpg`, `133.jpg`, `134.jpg`, `135.jpg`, `136.jpg`, `137.jpg`, `144.jpg`, `145.jpg`, `146.jpg`, `147.jpg`, `148.jpg`, `152.jpg`, `153.jpg`, `158.jpg`, `159.jpg`, `160.jpg`, `162.jpg`, `165.jpg`.
- `assets/roles/` (all imported): `damage.png`, `healer.png`, `tank.png`.
- `assets/sounds/` (all runtime-referenced): `manual-recording-error.mp3`, `manual-recording-start.mp3`, `manual-recording-stop.mp3`.
- `assets/icon/` (source-referenced): `death.png` by the raid-composition/player views, `large-icon.png` by the application status card, and `small-icon.png` by the main process and table cells.
- `assets/icon/` (packaged but no direct source reference found): `alt-icon.png`, `chest.png`, `clip-icon.png`, `ctrl-icon.png`, `dagger.png`, `discord-icon.png`, `dragon.png`, `dungeon.png`, `error-icon.png`, `five-people.png`, `flag.png`, `log-icon.png`, `saving-icon.png`, `settings-icon.png`, `swords.png`, `three-people.png`, `two-people.png`, `update.png`, `watch-icon.png`, `wifi.png`, `wowNotFound.png`, `youtube.png`.
- Outside the 125: root `assets/icon.png`, referenced by `install.sh:131`.

Evidence: imports and maps in `src/renderer/images.ts:3-212`, sounds in `src/renderer/sounds.ts` and `src/parsing/LogHandler.ts:517-540`, icon consumers found by exact-path search in `src/renderer/components/RaidComp.tsx`, `src/renderer/VideoPlayer.tsx`, `src/renderer/containers/ApplicationStatusCard/ApplicationStatusCard.tsx`, `src/renderer/components/Tables/Cells.tsx`, and `src/main/main.ts`; packaging includes the asset glob. Per-file hashes were reviewed locally but are not committed because hashes do not prove provenance.

No asset-specific provenance or redistribution notice was found for any raster or sound above. Therefore none is approved for native redistribution merely because it is imported or packaged. Use stock symbolic icons where the binding brief permits; otherwise obtain maintainer/provenance approval or an approved replacement before shipping.

## Canonical project license: hard block

- `LICENSE` begins with GNU GPL version 2 text (SHA-256 `1327219ca4c880c35fba4456eab11c59ad51f708403357c38b09bc267167308c`).
- `package.json:19` says `Creative Commons Attribution-NonCommercial` (package SHA-256 `f19b94564034aa1d5f9dfc666ec0bdea40c20d029c57570586454dbf24d34d2a`). That is not an SPDX identifier and conflicts with the repository license file.
- No maintainer decision was supplied. An agent cannot select or reconcile these terms.
- Result: `BLOCKED(license decision)`. WR-002 must not select shipping dependencies or publish Flatpak metadata until a maintainer records the canonical SPDX expression and confirms dependency compatibility.

## Approval

- Canonical SPDX: pending maintainer.
- Feature/assets redistribution approval: pending maintainer.

## Skipped (YAGNI)

- No speculative redrawing or generated replacement art: native UI should use stock symbolic icons until provenance is approved.
