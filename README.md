# relais

Relais is a coding-agent execution companion: it chooses a model and reasoning
effort for a bounded coding task, supplies authoritative context, verifies the
result, and escalates when necessary. Its objective is to reduce total cost
per accepted change while preserving an explicit quality bar.

Status: in development against the target specification in `docs/SPEC.md`. The
commands, schemas and integrations there are proposed interfaces, not shipped
features. Relais is a working name; package availability has not been checked.

## Build

```sh
make check   # toolchain lint test msrv
make build
```
