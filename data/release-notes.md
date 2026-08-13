# Release notes

Commit subjects per release, written by `scripts/generate-release-notes.sh` and
compiled into the binary: the "What's new" dialog reads the section matching the
running version. Only `## <version>` headings and `- ` lines are parsed.

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
