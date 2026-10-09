//! The askpass helper as a standalone program, for integration tests: the
//! test binary can't be `SSH_ASKPASS` (libtest parses its arguments).
//! Tests point `ION_ASKPASS_PROGRAM` at this example.

fn main() {
    std::process::exit(ion_remote::askpass_main().unwrap_or(2));
}
