# Ghostless JSON contracts

`contracts.schema.json` uses JSON Schema Draft 2020-12. Each file format has a
named subschema under `$defs`. For bridge exchanges, validate the request as
`BridgeRequest`, the reply as `BridgeResponse`, and a successful reply's
`result` as `<method>Result` using the originating request's method.

| File | Subschema |
| --- | --- |
| `manifest.json` | `Manifest` |
| `voice-stats.json` | `CompiledStats` |
| `session.json` | `SessionMeta` |
| `receipt.json` | `SessionReceipt` |
| `rules.json` | `RulesState` |
| `checks.json` | `ChecksState` |
| `meta.json` | `EditMeta` |

`valid.json` captures Python outputs from a synthetic writing project.
It includes a finished receipt with open session metadata, overlapping
hygiene decisions, and confirm requests that clear a section scope.
`invalid.json` contains malformed payloads. Python checks the complete
contracts and Rust checks the typed fields consumed by the editor.
`session-ids.json` defines shared acceptance vectors for both runtimes.
