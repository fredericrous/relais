//! Context packages (SPEC §7, §18).
//!
//! The manifest carries contract hash, base SHA, policy hash, tool
//! versions, source paths and fingerprints, architecture evidence and the
//! verification plan. Workers get the objective, acceptance criteria,
//! necessary constraints, a small set of entry points and exact failure
//! evidence — large files and logs are referenced by path and range.
//! Required constraints that exceed the context budget are a sizing
//! problem, never silently truncated. aval verdicts (active, undecided,
//! contradiction, retired, unknown) and aval tool failures stay distinct;
//! only a contradiction blocks affected work, and a missing answer gates
//! only tasks that depend on it.
