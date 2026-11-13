//! MIR analyses (ADR 0033, 0093): checks that read the MIR of the whole
//! program, `core` included, after it is built and before the parts that
//! dumps leave out are left out. They name only `mir` and the shared modules
//! (ADR 0063), and report through the diagnostics sink like every layer.
//!
//! Two kinds of check: the analyses of one body (`check_body`, ADR 0099),
//! which `query` runs on each body as it is built, and those of the whole
//! program (`check`), which the driver runs on the assembled MIR. The first are
//! the analysis of moves (ADR 0097), the borrow check (ADR 0106, 0107) and the
//! analysis of linearity (ADR 0102, 0103); the second, the rule of shared
//! statics (ADR 0078, 0093) and the conflicts of a call with what its callee
//! reaches (ADR 0111, 0112).

mod bits;
mod borrows;
mod call_borrows;
mod call_graph;
mod linear;
mod moves;
mod shared_statics;

use crate::diag::Diagnostics;
use crate::mir::{Body, Mir};

/// Runs every analysis of one body, reports what they find, and says whether
/// the body passed (ADR 0099, 0107). The body is one the validator accepted.
/// The borrow check and the analysis of linearity run only on a body that
/// passed the analysis of moves, so that a use after a move is not also
/// reported as a borrow in conflict or a value never consumed (ADR 0103); both
/// run on such a body, so that it reports the errors of each.
pub fn check_body(body: &Body, diags: &mut Diagnostics) -> bool {
    if !moves::check(body, diags) {
        return false;
    }
    let borrows = borrows::check(body, diags);
    let linear = linear::check(body, diags);
    borrows && linear
}

/// Runs every analysis over `mir`, reporting the errors it finds.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    shared_statics::check(mir, diags);
    call_borrows::check(mir, diags);
}
