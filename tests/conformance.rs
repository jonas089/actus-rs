//! Official ACTUS conformance tests. Skipped (with a note) if the test files are not available.

use actus_rs::conformance::run_suite;

fn check(ct: &str) {
    let Some((passed, total, failures)) = run_suite(ct) else {
        eprintln!("actus-tests not found; skipping {ct} (set ACTUS_TESTS_DIR)");
        return;
    };
    if !failures.is_empty() {
        panic!("{ct}: {passed}/{total} passed\n{}", failures.join("\n"));
    }
}

macro_rules! suites {
    ($($name:ident),*) => { $( #[test] fn $name() { check(stringify!($name)); } )* };
}

suites!(pam, clm, ump, csh, stk, com);
