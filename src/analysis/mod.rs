//! MIR analyses (ADR 0033, 0093): checks that read the MIR of the whole
//! program, `core` included, after it is built and before the parts that
//! dumps leave out are left out. They name only `mir` and the shared modules
//! (ADR 0063), and report through the diagnostics sink like every layer.
//!
//! Two kinds of check: the analyses of one body (`check_body`, ADR 0099),
//! which `query` runs on each body as it is built, and those of the whole
//! program (`check`), which the driver runs on the assembled MIR. The first is
//! the analysis of moves (ADR 0097); the second, the rule of shared statics
//! (ADR 0078, 0093).

mod moves;
mod shared_statics;

use crate::diag::Diagnostics;
use crate::mir::{Body, Mir};

/// Runs every analysis of one body, reports what they find, and says whether
/// the body passed (ADR 0099). The body is one the validator accepted.
pub fn check_body(body: &Body, diags: &mut Diagnostics) -> bool {
    moves::check(body, diags)
}

/// Runs every analysis over `mir`, reporting the errors it finds.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    shared_statics::check(mir, diags);
}
