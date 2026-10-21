//! The NLI cell says «looked and found no model» for the one state that means it,
//! and nothing before it looked.
//!
//! `cognitive::nli::resolved_without_a_model` reads a process-wide `OnceLock`,
//! and the quality judge found it unjudged: replacing it with `false` left every
//! test green (`src/cognitive/nli.rs:130`, `QG_BASE=origin/main`, 2026-10-20). The
//! pure half, `found_no_model`, has a table in the unit tests; what no test did was
//! put the live cell in `Unavailable` and ask the reader. `cargo mutants -- --lib`
//! cannot: the unit tests of nli.rs rely on the cell being unresolved («nothing in
//! this binary calls `enabled()`»), so resolving it there would make them depend on
//! test order. This file is its own process, which is the only place the cell can be
//! driven through both of its states with nobody else watching.
//!
//! `/health` reads this to tell a machine with no NLI model («restart it to load the
//! one installed since») from a model that loaded and from one that failed to open
//! (v045_health_names_the_model_that_would_not_open reaches that third state). K4's
//! defect was a `loaded` for a cell that had looked and found none.
//!
//! «Unavailable» is made here without a model and without ONNX Runtime: HOME and
//! USERPROFILE point at an empty directory, so no cache holds a model, and
//! CUBA_NLI_PATH is not defined. Nothing is downloaded and nothing is loaded.
//!
//! One test, and it has to stay one: the cell belongs to the process, and a second
//! test here would find it already resolved by the first.

use memory_industry::cognitive::nli;

#[test]
fn a_cell_that_looked_and_found_no_model_says_so_and_before_looking_says_nothing() {
    let home =
        std::env::temp_dir().join(format!("memory-industry-nli-cell-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("the test owns this directory");
    // SAFETY: this binary has one test, so nothing else reads or writes the
    // environment while these run; the harness thread that runs it is the only one.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::remove_var("CUBA_NLI_PATH");
    }

    assert!(
        !nli::available(),
        "an empty home holds no NLI model: if this says true the fixture is not empty and \
         what follows would load a model instead of finding none"
    );

    // Before anything looked: nobody found anything, so there is nothing to report.
    assert!(
        !nli::status_resolved(),
        "nothing in this process has looked for an NLI model yet"
    );
    assert!(
        !nli::resolved_without_a_model(),
        "a cell that has not looked cannot have found no model: saying so would put \
         `absent` in /health over an NLI that simply has not been asked yet"
    );

    // Looking: `enabled()` resolves the cell, and with no model it resolves to Unavailable.
    assert!(
        !nli::enabled(),
        "with no model on disk NLI cannot be enabled"
    );
    assert!(
        nli::status_resolved(),
        "enabled() looks, so the cell is resolved after it"
    );
    assert!(
        nli::resolved_without_a_model(),
        "the cell looked, found no model and will not look again: if this says false, /health \
         reads a model installed afterwards as `loaded` for an NLI that no session ever opened \
         (the defect K4 closed, with nothing left in the suite to notice it coming back)"
    );
    assert_eq!(
        nli::failure_reason(),
        None,
        "no model is an absence and not a failure: a reason here would put it in /health as one"
    );

    let _ = std::fs::remove_dir_all(&home);
}
