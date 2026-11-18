//! OKF v0.2 conformance gate tests (plan 04.1-01, OKF-01/03/05).
//!
//! T8 `okf_gate_rejects` pins the §11 rule port (`vault::okf::check_page`,
//! the Rust port of B3Wiki `scripts/okflint.py`): one violation per rule
//! class (E1, E2, W2, E5, W1, W3→E), zero on a clean page.

use saussunet_core::vault::okf;

/// A fully conformant generated-page frontmatter (D-06 key order, D-08
/// offset-bearing timestamps, D-13 sources shape).
fn clean_yaml() -> String {
    "type: Concept
title: Clean Page
description: A clean page.
tags: [technology]
timestamp: 2026-11-18T10:00:00+00:00
generated:
  by: process:saussunet/0.1.0
  at: 2026-11-18T10:00:00+00:00
sources:
- id: foo
  resource: /raw/foo.md
  title: Foo
  last_modified: 2026-11-18T09:00:00+00:00
"
    .to_string()
}

fn check(fm_yaml: &str) -> Vec<String> {
    okf::check_page(fm_yaml, "", &okf::DEFAULT_TYPE_VOCAB)
}

/// Assert exactly one violation and that it names the expected rule id.
fn one(v: Vec<String>, rule: &str) {
    assert_eq!(
        v.len(),
        1,
        "expected exactly one {rule} violation, got {v:?}"
    );
    assert!(v[0].contains(rule), "violation {v:?} must name rule {rule}");
}

#[test]
fn okf_gate_rejects() {
    let clean = clean_yaml();
    assert_eq!(check(&clean), Vec::<String>::new(), "clean page must pass");

    // E1 — missing / unparseable frontmatter.
    one(check(""), "E1");
    one(check("type: [unclosed\n"), "E1");

    // E2 — empty or non-string `type`.
    one(check(&clean.replace("type: Concept", "type: \"\"")), "E2");
    one(check(&clean.replace("type: Concept", "type: [a, b]")), "E2");

    // W2 — timestamps without an explicit UTC offset (date-only and naive).
    one(
        check(&clean.replace(
            "timestamp: 2026-11-18T10:00:00+00:00",
            "timestamp: 2026-11-18",
        )),
        "W2",
    );
    one(
        check(&clean.replace(
            "timestamp: 2026-11-18T10:00:00+00:00",
            "timestamp: 2026-11-18T10:00:00",
        )),
        "W2",
    );
    // W2 — generated.at without offset.
    one(
        check(&clean.replace("  at: 2026-11-18T10:00:00+00:00", "  at: 2026-11-18")),
        "W2",
    );

    // E5 — `generated` not a mapping / missing `by`.
    one(
        check(&clean.replace(
            "generated:\n  by: process:saussunet/0.1.0\n  at: 2026-11-18T10:00:00+00:00\n",
            "generated: true\n",
        )),
        "E5",
    );
    one(
        check(&clean.replace("  by: process:saussunet/0.1.0\n", "")),
        "E5",
    );

    // W1 — generated.by violating the §7 actor shape.
    one(
        check(&clean.replace("by: process:saussunet/0.1.0", "by: saussu net!")),
        "W1",
    );

    // W3→E — sources[] entry without a resource.
    one(
        check(&clean.replace("  resource: /raw/foo.md\n", "")),
        "W3->E",
    );
}
