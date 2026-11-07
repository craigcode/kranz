use glib::variant::ToVariant;

// Run optimized: the original shared-reference out-argument can be optimized
// away, leaving a NULL pointer even though debug builds appear to work.
#[test]
fn variant_str_iter_all_affected_operations_preserve_strings() {
    let value = ["zero", "one", "two", "three", "four"].to_variant();
    let mut iter = value.array_iter_str().unwrap();
    assert_eq!(iter.next(), Some("zero"));
    assert_eq!(iter.next_back(), Some("four"));
    assert_eq!(iter.nth(1), Some("two"));
    assert_eq!(iter.nth_back(0), Some("three"));
    assert_eq!(iter.next(), None);
    assert_eq!(value.array_iter_str().unwrap().last(), Some("four"));
    assert_eq!(value.array_iter_str().unwrap().collect::<Vec<_>>(), ["zero", "one", "two", "three", "four"]);
    let empty = Vec::<String>::new().to_variant();
    assert_eq!(empty.array_iter_str().unwrap().next_back(), None);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "KranzSecurityBackportEnum")]
#[repr(i32)]
enum BackportEnum { First, Second }

#[test]
fn diagnostic_adapter_compiles_glib_derives() {
    use glib::prelude::*;
    assert_eq!(BackportEnum::Second.to_value().get::<BackportEnum>().unwrap(), BackportEnum::Second);
}
