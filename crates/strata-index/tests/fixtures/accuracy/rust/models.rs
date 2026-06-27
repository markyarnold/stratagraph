// Accuracy corpus (Rust plane, Slice 21): the data types whose methods the call
// sites in service.rs and macros.rs target. Two distinct types each define a
// method named `save` — the deliberate ambiguity the unknown-receiver fan-out
// rule must surface, never disambiguate.
mod models {
    pub struct User;

    impl User {
        pub fn save(&self) {}
    }

    pub struct Account;

    impl Account {
        pub fn save(&self) {}
    }

    pub struct Builder;

    impl Builder {
        // A repo-unique method name, the cross-module unique-name target.
        pub fn build_one(&self) {}
    }
}
