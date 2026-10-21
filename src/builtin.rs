//! The enums `Option(T)` and `Result(T, E)`, built into the compiler until
//! generics exist (ADR 0045). Shared vocabulary for the layers that report
//! their misuse, like `int`; it goes away when they move to `core` (M6).

/// Whether `name` is `Option` or `Result`, the built-in enums with type
/// arguments (ADR 0045).
pub(crate) fn is_builtin_enum(name: &str) -> bool {
    matches!(name, "Option" | "Result")
}

/// The help for a path to a variant of `Option` or `Result`, `name`, such
/// as `Option::None`: there is none, and the variant is named by the
/// expected type (ADR 0044, 0045). `variant` is the name written after
/// `::`, shown if it is one of theirs.
pub(crate) fn builtin_variant_help(name: &str, variant: &str) -> String {
    let (written, variants): (&str, &[&str]) = if name == "Option" {
        ("an `Option(T)`", &["None", "Some"])
    } else {
        ("a `Result(T, E)`", &["Ok", "Err"])
    };
    let variant = if variants.contains(&variant) {
        variant
    } else {
        variants[0]
    };
    format!(
        "the variants of `{name}` have no path: write `.{variant}` where {written} is expected, and that type gives it its enum (ADR 0044, 0045)"
    )
}
