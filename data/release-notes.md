# Release notes

Commit subjects per release, written by `scripts/generate-release-notes.sh` and
compiled into the binary: the "What's new" dialog reads the section matching the
running version. Only `## <version>` headings and `- ` lines are parsed.

## 1.0.9
- Claim the single instance before touching storage
- Release the spell borrow before clearing it
- Skip hidden meter refreshes and virtualize the histories
- Defer seeks until Clapper reports the item ready
- Fold completions into the library index instead of rescanning

## 1.0.8
- Correct the documented application sizes
- Replace the GSR child when a capture produces no video file
- Stop the GSR recording toggle from desyncing
- Prove the next recording saves after a desync
- Use as_chunks for the MD5 block loop

## 1.0.7
- Add focused combat review tools
- Add resizable combat damage meter
- local dmg meter improvements
- Improve local damage meter accuracy and menus
- Sync local meter fights to capturing player combat
- Keep group meter fight active after host death
- Simplify combat meter navigation controls
- Add damage taken and death meter views
- Scope death logs to their fight
- Draw death log rows as draining health bars
- Fix meter row clicks and solidify bar fills
- Add per-spell statistics and target split to meter
- Add casts meter view, overheal detail, and seekable meter rows
- Remove meter row hover highlight and stray images
- Animate meter bar fills between samples
- Strip UTC-offset suffix from combat log timestamps before parsing
- Add spell icons and stable tooltips
- order by spell %
- remove casts in non detailed view
- Document the local combat meter

## 1.0.6
- Add Midnight patch 12.1 content IDs

## 1.0.5
- Clear stale screen-capture errors after recovery

## 1.0.4
- Harden replay artifact correlation

## 1.0.3
- Refuse the release smoke test when the app is really installed
- Make install.sh the one-command installer and rewrite the README
- Warn about missing capture portal and prerequisites
- Prevent media worker restart deadlock
- Reduce hot-path allocation overhead
- Protect existing files during folder probes
- Remove low-value helpers and tests

## 1.0.2
- Generate per-release commit notes and gate releases on them
- Show a What's new dialog after an update
