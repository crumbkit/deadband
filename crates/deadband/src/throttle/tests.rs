//! Tests of lowering the priority of `deadband run`, which its job inherits.

use super::{lower_own_priority, own_priority};

#[test]
fn lowering_the_priority_sets_at_least_the_nice_value_and_never_raises_it() {
    let before = own_priority().expect("the priority reads");
    let target = before.max(5);
    lower_own_priority(target).expect("the priority lowers");
    assert_eq!(own_priority().expect("the priority reads"), target);
    lower_own_priority(0).expect("a lower nice value is left alone");
    assert_eq!(own_priority().expect("the priority reads"), target);
}
