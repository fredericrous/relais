//! Task contracts (SPEC §4): validated, frozen, hashed.
//!
//! A contract is checked against `schema_version` 1 and hashed before the
//! first worker; objective, scope, acceptance criteria and budget are
//! immutable afterwards. Changing any of them creates a new revision that
//! requires renewed verification. Unknown fields are rejected to catch
//! misspelled controls. `kind` is `inspect` (evidence criteria, no patch)
//! or `change` (bounded write scope required). `base_ref` resolves once to
//! a commit SHA. Empty architecture keys mean "no explicit mapping
//! supplied", not "architecture does not apply".
