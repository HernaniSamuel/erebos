//! MIR analyses (ADR 0033, 0093): checks that read the MIR of the whole
//! program, `core` included, after it is built and before the parts that
//! dumps leave out are left out. They name only `mir` and the shared modules
//! (ADR 0063), and report through the diagnostics sink like every layer.
//!
//! Two kinds of check: the analyses of one body (`check_body`, ADR 0099),
//! which `query` runs on each body as it is built, and those of the whole
//! program (`check`), which the driver runs on the assembled MIR. The first are
//! the analysis of moves (ADR 0097) and the analysis of linearity (ADR 0102,
//! 0103); the second, the rule of shared statics (ADR 0078, 0093).

mod bits;
mod linear;
mod moves;
mod shared_statics;

use crate::diag::Diagnostics;
use crate::mir::{Body, Mir};

/// Runs every analysis of one body, reports what they find, and says whether
/// the body passed (ADR 0099). The body is one the validator accepted. The
/// analysis of linearity runs only on a body that passed the analysis of
/// moves, so that a use after a move is not also reported as a value never
/// consumed (ADR 0103).
pub fn check_body(body: &Body, diags: &mut Diagnostics) -> bool {
    moves::check(body, diags) && linear::check(body, diags)
}

/// Runs every analysis over `mir`, reporting the errors it finds.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    shared_statics::check(mir, diags);
}
