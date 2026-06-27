//! Shape types with impl methods. Two distinct types each define an `area`
//! method (the unknown-receiver ambiguity) and a `scale` method; `Rectangle`
//! adds a repo-unique `perimeter`. Trait `Shape` is implemented by both.

pub struct Rectangle {
    pub w: f64,
    pub h: f64,
}

impl Rectangle {
    pub fn new(w: f64, h: f64) -> Rectangle {
        Rectangle { w, h }
    }

    pub fn area(&self) -> f64 {
        // self.<method> on the enclosing type -> Inferred (own-type method).
        self.normalized()
    }

    pub fn normalized(&self) -> f64 {
        self.w * self.h
    }

    pub fn scale(&mut self, factor: f64) {
        self.w *= factor;
        self.h *= factor;
    }

    pub fn perimeter(&self) -> f64 {
        // A repo-unique method name (cross-module unique-name target).
        2.0 * (self.w + self.h)
    }
}

pub struct Circle {
    pub r: f64,
}

impl Circle {
    pub fn new(r: f64) -> Circle {
        Circle { r }
    }

    pub fn area(&self) -> f64 {
        3.0 * self.r * self.r
    }

    pub fn scale(&mut self, factor: f64) {
        self.r *= factor;
    }
}

pub trait Shape {
    fn describe(&self) -> f64;
}

impl Shape for Rectangle {
    fn describe(&self) -> f64 {
        self.area()
    }
}

impl Shape for Circle {
    fn describe(&self) -> f64 {
        self.area()
    }
}
