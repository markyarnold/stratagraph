// A Rust source in the mixed-language fixture: a module whose struct method calls
// a same-file fn (Extracted 0.95) and a self-method (Inferred 0.80), and invokes a
// macro that must NOT become a call edge — linked entirely within the Rust
// (`rust`-tagged) resolution world, no cross-language edge to the TS/Python/C#
// planes this slice.
mod services {
    pub struct RustService;

    impl RustService {
        pub fn run(&self) -> i32 {
            // A macro invocation — NEVER a call edge (the load-bearing honesty pin).
            println!("running");
            // Same-file bare call → Extracted 0.95.
            let v = rust_helper();
            // self-receiver call to an own-type method → Inferred 0.80.
            self.compute(v)
        }

        fn compute(&self, x: i32) -> i32 {
            x + 1
        }
    }

    pub fn rust_helper() -> i32 {
        7
    }
}
