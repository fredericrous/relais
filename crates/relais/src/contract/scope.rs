//! Glob predicates over a contract's declared write scope (SPEC §6).
//!
//! Two questions that look alike and are not. COULD these two patterns
//! share a path — the over-approximation risk floors and architecture
//! mappings are computed with, before any diff exists — and IS every
//! path of one inside the other, the containment a deterministic recipe
//! needs before it may claim to cover a task. Overlap is symmetric,
//! containment is not, and using one for the other has cost this crate a
//! finding in each direction.
//!
//! They live beside the contract, whose `write_scope` they read, so that
//! `policy` and `context` can ask the question without either one
//! depending on the router that used to own it.

use super::{TaskContract, WriteScope};

/// Could one path match BOTH globs? Routing happens before any diff
/// exists, so floors are computed from the DECLARED scope patterns
/// (SPEC §6). The answer is exact for `**` (zero or more segments) and
/// conservative inside a segment (a `*` segment is judged by its literal
/// prefix and suffix only), so an over-approximation costs an escalation
/// tier while an under-approximation would route a sensitive write to a
/// cheap model — the failure the spec forbids.
///
/// It used to answer "yes" to anything when either side began with `**`,
/// which made the init template's `**/trust/**` rule floor a contract
/// scoped to `docs/README.md`. A directory scope such as `src/**` still
/// takes that floor, correctly: `src/trust/x` matches both.
pub(crate) fn scope_could_touch(scope: &str, pattern: &str) -> bool {
    let segments = |glob: &str| -> Vec<String> {
        glob.trim_start_matches("./")
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect()
    };
    globs_overlap(&segments(scope), &segments(pattern))
}

fn globs_overlap(a: &[String], b: &[String]) -> bool {
    match (a.first(), b.first()) {
        (None, None) => true,
        (Some(x), _) if x == "**" => {
            globs_overlap(&a[1..], b) || (!b.is_empty() && globs_overlap(a, &b[1..]))
        }
        (_, Some(y)) if y == "**" => {
            globs_overlap(a, &b[1..]) || (!a.is_empty() && globs_overlap(&a[1..], b))
        }
        (Some(x), Some(y)) => segments_overlap(x, y) && globs_overlap(&a[1..], &b[1..]),
        _ => false,
    }
}

/// Two single segments: equal, or wildcarded with compatible literal
/// prefix and suffix. `*.rs` and `main.rs` overlap; `a*` and `b*` do not.
fn segments_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let wild = |s: &str| s.contains('*') || s.contains('?') || s.contains('[');
    if !wild(a) && !wild(b) {
        return false;
    }
    // A literal segment is its own prefix AND suffix; a wildcarded one
    // contributes the text before its first and after its last wildcard.
    let literal = |s: &str| -> (String, String) {
        if !wild(s) {
            return (s.to_string(), s.to_string());
        }
        let first = s.find(['*', '?', '[']).unwrap_or(s.len());
        let last = s.rfind(['*', '?', ']']).map_or(s.len(), |i| i + 1);
        (s[..first].to_string(), s[last.max(first)..].to_string())
    };
    let (pa, sa) = literal(a);
    let (pb, sb) = literal(b);
    let prefixes = pa.starts_with(&pb) || pb.starts_with(&pa);
    let suffixes = sa.ends_with(&sb) || sb.ends_with(&sa);
    prefixes && suffixes
}

/// Could any pattern of this declared scope share a path with `pattern`?
pub fn scope_could_touch_any(scope: &WriteScope, pattern: &str) -> bool {
    scope
        .patterns()
        .iter()
        .any(|declared| scope_could_touch(declared, pattern))
}

/// The same question about a contract: an inspect contract declares no
/// scope and so touches nothing.
pub fn write_scope_could_touch(contract: &TaskContract, pattern: &str) -> bool {
    contract
        .write_scope()
        .is_some_and(|scope| scope_could_touch_any(scope, pattern))
}

/// Is every path matched by `scope` also matched by `cover`? This is
/// CONTAINMENT, not overlap, and it is deliberately incomplete: glob
/// containment in general is not something to decide inside a routing
/// function, so anything this cannot decide is "not contained".
///
/// `scope_could_touch` answers a different question — could these two
/// patterns share a path — and using it here made a recipe "fully cover"
/// a task it covered almost none of: a contract scoped `**` overlaps a
/// `docs/**` recipe, so the whole repository was routed by the docs
/// recipe's tier (finding B13). The two predicates are not
/// interchangeable in either direction: overlap is symmetric and
/// containment is not.
///
/// Decidable cases:
/// - `cover` is `**`: it matches every path, so anything is contained.
/// - `cover` ends in `/**` and its leading segments are all literal:
///   `scope` is contained when its own segments begin with exactly those
///   literals and it has at least one segment more.
/// - the two patterns are identical.
///
/// Everything else — a wildcard anywhere in the cover's prefix
/// (`**/trust/**`, `docs/*/**`), a cover with no trailing `**`, a scope
/// shorter than the cover's prefix — is undecidable here and answers
/// false, which costs a recipe and never grants one.
pub(crate) fn scope_contained_in(scope: &str, cover: &str) -> bool {
    let segments = |glob: &str| -> Vec<String> {
        glob.trim_start_matches("./")
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect()
    };
    let scope = segments(scope);
    let cover = segments(cover);
    if cover.is_empty() {
        return false;
    }
    if cover.len() == 1 && cover[0] == "**" {
        return true;
    }
    if scope == cover {
        return true;
    }
    let Some((last, prefix)) = cover.split_last() else {
        return false;
    };
    if last != "**" || prefix.is_empty() {
        return false;
    }
    let wild = |segment: &String| segment.contains(['*', '?', '[']);
    if prefix.iter().any(wild) {
        return false;
    }
    scope.len() > prefix.len() && scope.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_exact_for_double_star_and_conservative_within_a_segment() {
        // The init template's rule against a single-file scope elsewhere.
        assert!(!scope_could_touch("docs/README.md", "**/trust/**"));
        assert!(!scope_could_touch("src/main.rs", "**/trust/**"));
        // …and against directory scopes that genuinely could reach it.
        assert!(scope_could_touch("src/**", "**/trust/**"));
        assert!(scope_could_touch("crates/amont/trust/**", "**/trust/**"));
        assert!(scope_could_touch("**", "crates/other/**"));
        assert!(!scope_could_touch("**/trust/**", "src/main.rs"));
        assert!(scope_could_touch("**/*.rs", "src/main.rs"));
        assert!(!scope_could_touch("**/*.rs", "src/main.py"));
        assert!(!scope_could_touch("crates/a/**", "crates/b/**"));
        assert!(scope_could_touch("crates/a*/**", "crates/ab/**"));
        assert!(!scope_could_touch("crates/a*/**", "crates/b/**"));
        assert!(scope_could_touch("./src/**", "src/lib.rs"));
    }

    /// B13: coverage was tested with `scope_could_touch`, a symmetric
    /// could-intersect predicate, so a contract that may write ANYWHERE
    /// was "fully covered" by a recipe scoped to `docs/**`.
    #[test]
    fn recipe_coverage_is_containment_not_overlap() {
        assert!(
            !scope_contained_in("**", "docs/**"),
            "a scope over the whole repository is not inside docs/"
        );
        assert!(scope_contained_in("docs/a/**", "docs/**"));
        assert!(
            !scope_contained_in("docs/**", "docs/a/**"),
            "containment is not symmetric"
        );
        assert!(scope_contained_in("docs/guide.md", "docs/**"));
        assert!(scope_contained_in("./docs/guide.md", "docs/**"));
        assert!(scope_contained_in("anything/at/all", "**"));
        assert!(scope_contained_in("docs/**", "docs/**"));
        // The cover's own wildcards make containment undecidable here.
        assert!(!scope_contained_in("crates/amont/trust/x", "**/trust/**"));
        assert!(!scope_contained_in("docs/a/b", "docs/*/**"));
        // A cover that is not a directory glob covers only itself.
        assert!(!scope_contained_in("docs/guide.md", "docs"));
        assert!(!scope_contained_in("docs", "docs/**"));
        assert!(!scope_contained_in("docsets/guide.md", "docs/**"));
        // The old overlap predicate said yes to the first two.
        assert!(scope_could_touch("**", "docs/**"));
        assert!(scope_could_touch("docs/**", "docs/a/**"));
    }
}
