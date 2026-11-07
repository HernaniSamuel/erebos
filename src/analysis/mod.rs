//! MIR analyses (ADR 0033, 0093): checks that read the MIR of the whole
//! program, `core` included, after it is built and before the parts that
//! dumps leave out are left out. They name only `mir` and the shared modules
//! (ADR 0063), and report through the diagnostics sink like every layer.
//!
//! The one check so far is the rule of shared statics (ADR 0078, 0093).

mod shared_statics;

use crate::diag::Diagnostics;
use crate::mir::Mir;

/// Runs every analysis over `mir`, reporting the errors it finds.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    shared_statics::check(mir, diags);
}
