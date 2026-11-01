//! Generated programs (ADR 0068). This file holds the test of the generator
//! itself; the programs are run against their oracles by later steps.

mod common;

use std::collections::HashSet;
use std::path::Path;

use common::generator::{self, Feature, Uses};
use erebos::driver::{self, OptLevel, Stage};

/// How many seeds the test of the generator compiles.
const SEEDS: u64 = 300;

#[test]
fn generator_makes_valid_programs() {
    assert_eq!(Feature::ALL.len(), Feature::COUNT);

    let mut uses = Uses::default();
    let mut sources = HashSet::new();
    for seed in 0..SEEDS {
        let program = generator::generate(seed);

        // The same seed gives the same text.
        assert_eq!(
            program.source,
            generator::generate(seed).source,
            "seed {seed} gave two different programs"
        );
        sources.insert(program.source.clone());

        if let Err(rendered) = driver::dump(
            Path::new("generated.ers"),
            &program.source,
            Stage::Mir,
            OptLevel::None,
            false,
        ) {
            panic!(
                "the generated program of seed {seed} is not valid:\n{rendered}\n--- program ---\n{}",
                program.source
            );
        }
        uses = uses.union(program.uses);
    }
    assert!(
        sources.len() as u64 > SEEDS * 9 / 10,
        "the seeds gave only {} different programs",
        sources.len()
    );

    // Every operator, type and construct of ADR 0068, section 1, is in at
    // least one program of the run.
    let missing: Vec<_> = Feature::ALL.iter().filter(|f| !uses.has(**f)).collect();
    assert!(
        missing.is_empty(),
        "no program of {SEEDS} seeds uses: {missing:?}"
    );
}
