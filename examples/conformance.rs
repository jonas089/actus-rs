//! Print a pass/fail summary of the official ACTUS tests, or details of one case:
//! `cargo run --example conformance [type [case-id]]`

use actus_rs::conformance::{compare, load, run_case, run_suite};

const TYPES: &[&str] = &[
    "pam", "lam", "lax", "nam", "ann", "clm", "ump", "csh", "stk", "com", "fxout", "swppv", "swaps", "capfl", "optns",
    "futur", "ceg", "cec",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [ct, id] => {
            let cases = load(ct).expect("test file");
            let (_, case) = cases.iter().find(|(k, _)| k == id).expect("case");
            match run_case(case) {
                Ok(res) => {
                    for r in &res {
                        let st = &r.state;
                        let g = |n: &str| {
                            actus_rs::state::StateVariable::from_name(n)
                                .and_then(|v| st.number(v))
                                .map(|x| format!("{:.6}", x.to_f64()))
                                .unwrap_or("-".into())
                        };
                        println!(
                            "{} {:5} payoff={:>16.6} nt={} ipnr={} ipac={}",
                            r.event.time,
                            r.event.event_type.to_string(),
                            r.payoff.to_f64(),
                            g("notionalPrincipal"),
                            g("nominalInterestRate"),
                            g("accruedInterest")
                        );
                    }
                    println!("---- expected");
                    for e in case["results"].as_array().unwrap() {
                        println!("{e}");
                    }
                    if let Err(e) = compare(case, &res) {
                        println!("---- MISMATCH\n    {e}");
                    }
                }
                Err(e) => println!("error: {e}"),
            }
        }
        _ => {
            let types: Vec<&str> = if let [ct] = args.as_slice() { vec![ct.as_str()] } else { TYPES.to_vec() };
            let (mut p, mut t) = (0, 0);
            for ct in types {
                match run_suite(ct) {
                    Some((passed, total, failures)) => {
                        p += passed;
                        t += total;
                        println!("{ct:6} {passed:3}/{total:3}");
                        if args.len() == 1 {
                            for f in failures {
                                println!("  {f}");
                            }
                        }
                    }
                    None => println!("{ct:6} (no test file)"),
                }
            }
            println!("TOTAL  {p}/{t}");
        }
    }
}
