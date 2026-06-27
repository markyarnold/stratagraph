// Accuracy corpus (Rust plane): the call sites exercising each resolution rule.
// Every confidence is band-capped; a call the heuristic cannot point anywhere is
// surfaced (counted unresolved), never invented.
mod service {
    pub struct Service;

    impl Service {
        pub fn run(&self, acct: Account) {
            // 1. Same-file bare call → Extracted 0.95.
            helper();
            // 2. self-receiver to an own-type method → Inferred 0.80.
            self.compute();
            // 3. Unique cross-module name (`build_one` exists once repo-wide) →
            //    Inferred 0.80.
            build_one();
            // 4. Unknown-receiver `acct.save()` with TWO repo-wide `save` methods
            //    (User, Account) → Ambiguous fan-out (0.35), never a confident pick.
            //    The receiver is a *value*, so its concrete type is unknown.
            acct.save();
            // 4b. The SAME `save` method, but TYPE-QUALIFIED: `User::save()` names the
            //     type explicitly, so it resolves to EXACTLY User::save → Inferred
            //     0.80 (slice 23). Contrast with 4: an instance receiver is ambiguous,
            //     an explicit `Type::` qualifier is precise. (Cross-file → Inferred,
            //     not Extracted.)
            User::save();
            // 5a. Unknown bare name — no def anywhere → no edge (unresolved).
            ghost();
            // 5b. Trait dispatch on an unknown receiver with no `absent` method
            //     anywhere → no edge (unresolved); never an invented dispatch.
            acct.absent();
        }

        fn compute(&self) {}
    }

    fn helper() {}
}
