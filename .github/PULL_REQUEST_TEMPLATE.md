## Linked issue / wave

<!-- Issue number and hardening or feature wave, if any. -->

## What changed

## Schema, bridge, and packaging impact

<!-- Call out SCHEMA.md, bridge methods, and npm tarball contents. Write "none" when nothing user-facing moved. -->

## Local checks

- [ ] `./scripts/check`
- [ ] `bash tests/run.sh`
- [ ] `python tests/smoke_package.py`

## CHANGELOG

<!-- Required when the change is user-facing. Otherwise say "not user-facing". -->

- [ ] New guardrails/bugfixes were pushed as high up the determinism hierarchy as practical (code/types -> lint/CI -> verification tests -> prose only if non-deterministic).
