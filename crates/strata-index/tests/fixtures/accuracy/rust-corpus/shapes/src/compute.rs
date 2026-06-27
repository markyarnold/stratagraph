//! Compute helpers: same-file calls (Extracted), cross-module type-qualified
//! constructor + method calls (resolved), unknown-receiver method fan-outs
//! (Ambiguous), and a unique cross-module name (Inferred).

use crate::models::{Circle, Rectangle, Shape};

pub fn build_rectangle(w: f64, h: f64) -> Rectangle {
    // Type-qualified `Rectangle::new(...)` — resolves to exactly Rectangle::new
    // (cross-file -> Inferred, type-qualified rule, slice 23).
    Rectangle::new(w, h)
}

pub fn build_circle(r: f64) -> Circle {
    // Type-qualified `Circle::new(...)` -> Inferred.
    Circle::new(r)
}

pub fn total_area(a: &Rectangle, b: &Circle) -> f64 {
    // `.area()` on two receivers: two `area` methods repo-wide (Rectangle,
    // Circle) -> Ambiguous fan-out for the heuristic. rust-analyzer resolves
    // each to the receiver's concrete type.
    a.area() + b.area()
}

pub fn grow(a: &mut Rectangle, b: &mut Circle, factor: f64) {
    // `.scale()` on two receivers -> Ambiguous fan-out; rust-analyzer narrows.
    a.scale(factor);
    b.scale(factor);
}

pub fn fence_length(rect: &Rectangle) -> f64 {
    // `.perimeter()` is unique to Rectangle repo-wide, but an instance-receiver
    // method call is ALWAYS Ambiguous for the heuristic (no receiver type) — a
    // single-candidate Ambiguous rust-analyzer confirms exactly.
    rect.perimeter()
}

pub fn describe_both(a: &Rectangle, b: &Circle) -> f64 {
    // Trait-method dispatch `.describe()` on two trait impls (Rectangle, Circle)
    // -> Ambiguous fan-out; rust-analyzer resolves each to its impl.
    a.describe() + b.describe()
}

pub fn local_helper() -> f64 {
    // Same-file bare call -> Extracted.
    seed()
}

pub fn seed() -> f64 {
    1.0
}

pub fn run(w: f64, h: f64, r: f64) -> f64 {
    // Same-file bare calls -> Extracted.
    let rect = build_rectangle(w, h);
    let circ = build_circle(r);
    total_area(&rect, &circ)
}
