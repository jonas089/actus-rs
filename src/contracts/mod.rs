//! Contract type implementations.

pub mod ann;
pub mod capfl;
pub mod cec;
pub mod ceg;
pub mod clm;
pub mod com;
pub mod csh;
pub mod futur;
pub mod fxout;
pub mod lam;
pub mod lax;
pub mod loan;
pub mod nam;
pub mod optns;
pub mod pam;
pub mod stk;
pub mod swaps;
pub mod swppv;
pub mod ump;

// Contract types whose spec state tables are identical share one state struct (D10, revised).
// These aliases name the state of each lending type explicitly.
pub type PamState<const S: u32> = loan::LoanState<S>;
pub type LamState<const S: u32> = loan::LoanState<S>;
pub type LaxState<const S: u32> = loan::LoanState<S>;
pub type NamState<const S: u32> = loan::LoanState<S>;
pub type AnnState<const S: u32> = loan::LoanState<S>;
pub type UmpState<const S: u32> = loan::LoanState<S>;
