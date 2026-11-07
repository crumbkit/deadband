//! Tests of what the deadband library exposes.
#![cfg(test)]

#[test]
fn the_library_names_itself_deadband() {
    assert_eq!(deadband::NAME, "deadband");
}
