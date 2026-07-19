# WR-004: Combat-log reader and parser

## Goal

Incrementally read configured WoW combat logs and convert retained lines into small timestamped domain events. This ticket parses facts only; activity/session decisions belong to WR-005.

## Dependencies

WR-000 and WR-003 must be `DONE`.

## Owned files

- `native/src/logwatch.rs`
- `native/src/parser.rs` or `native/src/parser/`
- parser/log-reader tests in those modules
- shared WR-000 log fixtures only when an anonymization correction is required

## Interface

Implement concrete types, not parser traits/frameworks:

```rust
pub struct ParsedEvent {
    pub flavor: GameFlavor,
    pub occurred_at_ms: i64,
    pub event: CombatEvent,
}

pub struct LogTailer { /* path, identity, byte offset, incomplete bytes, time context */ }

impl LogTailer {
    pub fn open(path: PathBuf, flavor: GameFlavor) -> Result<Self, LogError>;
    pub fn poll(&mut self) -> Result<Vec<ParsedEvent>, LogError>;
}
```

`CombatEvent` contains only variants/fields consumed by WR-005 goldens: zone/map changes, encounter/challenge/arena/battleground/round start/end, combatant/player facts, deaths, and other precise retained transition data. Do not model every combat-log event or preserve raw comma fields after parsing.

## Implementation

1. Discover the active file using the exact directory/pattern/flavour rules in WR-000. Poll from the coordinator; do not create watcher threads or add a filesystem-watch dependency.
2. On first open, start at EOF for live use. Provide an explicit fixture/replay constructor that starts at byte zero; never infer test mode from the path.
3. Read appended bytes in bounded chunks with `std::io`. Retain only the final incomplete line between polls. Accept `\n` and `\r\n`; reject/record invalid UTF-8 without panicking.
4. Track file identity plus length. Handle only realistic rotation/truncation states observed in WR-000: switch to a newer active file or reset offset after same-file truncation. Do not implement recursive history import, arbitrary rename recovery, compression, or network filesystems.
5. Parse timestamps deterministically. WoW lines that omit a year use a `ParseTimeContext` derived from the file's recorded year/timezone rules in WR-000; fixture tests pass it explicitly. Never call “now” per line.
6. Split fields according to the actual quoted/escaped combat-log grammar in the retained fixtures. Reuse the current app's factual encounter/map/affix tables only where WR-000 approved source/license; normalize them once in Rust data, not behind lookup abstractions.
7. Unknown event names and malformed irrelevant lines are skipped. A malformed retained event returns a bounded diagnostic containing event name and line number/offset, never the whole potentially personal line. Deduplicate repeated diagnostics by `(kind, file)` in the coordinator rather than accumulating one per line.
8. Preserve event occurrence time through every emitted `ParsedEvent`; WR-005 uses it for replay lead-in and media-relative markers.

## Acceptance criteria

- Every WR-000 fixture produces the exact ordered parsed-event golden, including occurrence timestamps.
- A retained event split at every byte position across two reads parses exactly once after completion.
- Appending, CRLF, one rotation/truncation example, unknown events, invalid UTF-8, and one malformed retained event behave as specified without panic or duplicate events.
- Live open begins at EOF and replay open begins at zero.
- No activity state, recorder command, GTK type, background thread, filesystem watcher, or parser dependency is introduced.

## Tests

Use WR-000 fixtures. One loop over all split positions replaces dozens of hand-authored boundary tests. Add one table-driven event-variant test, one tail/rotation test, and one diagnostics test. Large throughput measurements belong to WR-015; do not add timing assertions or a second synthetic corpus here.

## Not in scope

Activity correlation, recording decisions, library scanning, importing old log history, supporting unretained events “just in case,” or optimizing before WR-015 identifies this path.
