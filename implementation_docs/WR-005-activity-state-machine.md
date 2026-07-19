# WR-005: Activity state machine and recording actions

## Goal

Translate timestamped parsed events into the exact automatic-recording actions and metadata/timeline recorded by WR-000, using deterministic GTK-free state.

## Dependencies

WR-003 and WR-004 must be `DONE`.

## Owned files

- `native/src/activity.rs`
- activity tests in that module
- WR-000 goldens only when correcting documented baseline evidence

## Interface

Implement one concrete engine, with per-flavour active state because multiple configured log directories may be polled:

```rust
pub struct ActivityEngine { /* retained state by flavor */ }

pub enum ActivityAction {
    Begin { draft: RecordingDraft, detected_at_ms: i64 },
    Update { id: RecordingId, item: TimelineItem },
    Complete { id: RecordingId, outcome: Outcome, ended_at_ms: i64 },
    Abandon { id: RecordingId, ended_at_ms: i64, reason: AbandonReason },
    Discard { id: RecordingId, reason: DiscardReason },
}

impl ActivityEngine {
    pub fn handle(&mut self, event: ParsedEvent, config: &ActivityConfig)
        -> Vec<ActivityAction>;
    pub fn force_end(&mut self, flavor: GameFlavor, occurred_at_ms: i64)
        -> Vec<ActivityAction>;
}
```

Use the smallest internal states needed by the goldens. Do not expose a generic workflow/state-machine framework. `RecordingDraft` contains enough stable identity/details to start capture and later build `LibraryEntry`; its occurrence start time is distinct from the later detection time.

## Behavior

1. Implement every activity/flavour path classified `KEEP`, including successful, loss/failure, timeout, reload/zone-change, overlapping/noisy events, and `Abandoned` outcomes exactly where WR-000 records them.
2. Compute `detected_at_ms - activity_started_at_ms` without wall-clock calls. WR-008 adds configured extra lead-in and clamps it to the recorder buffer.
3. Emit one `Begin` per logical activity. Duplicate/noisy starts update or are ignored according to the baseline; they never start a second recorder session.
4. Store complete metadata regardless of current UI marker preferences. Death, encounter-segment, and round-boundary preferences hide timeline presentation later; changing a preference must reveal historical markers without rerecording.
5. Build point/span timeline items with media-independent activity timestamps. WR-008 converts them to media offsets using the actual capture start/replay amount.
6. Apply current recording toggles and thresholds at the same stage as the baseline. If the current app buffers then discards below-threshold activities, preserve that observable behavior rather than inventing an earlier optimization.
7. Only the engine decides domain completion/abandon/discard. It does not spawn GSR, write sidecars, send UI messages, sleep, or retry.
8. `force_end` targets only that flavour's active automatic activity and emits the exact WR-000 forced outcome/end/overrun metadata before clearing it. With no matching active activity it returns no action. Manual/test stopping is coordinator-owned and does not fabricate an automatic activity event.
9. Keep category-specific detail construction close to the transition that has the data. Add a helper only when the same transformation is used in more than one retained path.

## Acceptance criteria

- Replaying each WR-000 activity fixture produces its exact action/metadata/timeline golden.
- Occurrence time, detection time, duration, outcome (including `Abandoned`), details, activity hash, player/combatant data, and ordered points/spans match.
- Duplicate and interleaved flavour events cannot create duplicate begins or terminate another flavour's active activity.
- Forced end produces the recorded final action/metadata once and cannot end the wrong flavour.
- Marker visibility config does not remove stored timeline data.
- The module is deterministic: no filesystem/process/GTK access, sleeps, global singleton, or direct `SystemTime::now()`.

## Tests

One golden replay per distinct WR-000 activity path is the main suite. Add focused invariants only if not covered: duplicate start, interleaved flavours, forced end/wrong flavour, and visibility settings preserving timeline data. Do not duplicate parser split/rotation tests or create permutations of irrelevant event fields.

## Not in scope

Manual/test recording commands, GSR control, conversion to media offsets, storage, UI labels, or speculative support for activity types absent from the signed parity matrix.
