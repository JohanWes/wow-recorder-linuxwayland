# Evidence report template

Create the report named by the ticket and keep it factual. Reports are implementation evidence, not narrative status updates.

```markdown
# <ticket>: <report title>

## Environment
- commit:
- OS/kernel/session:
- hardware/runtime/tool versions relevant to this ticket:

## Contract checked
- acceptance criterion → evidence location/result

## Commands and raw results
```text
<exact command>
<important output, artifact hash, or link/path to full log>
```

## Manual scenarios
| Scenario | Preconditions | Steps | Expected | Actual | Pass |
|---|---|---|---|---|---|

## Measurements
| Metric | Method | Samples | Median | Budget/baseline | Pass |
|---|---|---|---|---|---|

## Files/artifacts
- fixture, screenshot, trace, package, or log path plus purpose/hash

## Decisions and deviations
- decision, evidence, approver, and downstream ticket affected

## Skipped (YAGNI)
- one-line item and why no current behavior/gate requires it

## Known limitations
- observable limitation, impact, and owning follow-up/ticket; do not hide a failed acceptance criterion here

## Approval
- reviewer/maintainer, date, result
```

Omit sections that genuinely do not apply. Do not paste enormous logs, duplicate ticket prose, or manufacture screenshots/tests for pure code contracts. Store bulky evidence at a stable referenced path and include the command, hash, and relevant excerpt.

Measurement reports include raw samples and median, never only a rounded claim. Manual evidence names exact data and entry point. Any scope/dependency/license exception names its approver; an agent cannot self-approve it.
