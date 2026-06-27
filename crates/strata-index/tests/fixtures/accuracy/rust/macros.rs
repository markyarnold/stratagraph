// Accuracy corpus (Rust plane): the macros-are-NOT-calls honesty pin. Every macro
// invocation here would, if the expansion were guessed, produce a confident wrong
// call edge — so NONE may become a call. A macro invocation is a `macro_invocation`
// AST node, never a `call_expression`; the extractor drops it and never guesses the
// expansion. The one explicit fn call below is the only call site this file
// contributes.
mod macros {
    pub struct Worker;

    impl Worker {
        pub fn drive(&self) {
            // A std macro — must NOT be a call to `println`.
            println!("driving");
            // A collection macro — must NOT be a call to `vec`.
            let _ = vec![1, 2, 3];
            // An assertion macro — must NOT be a call to `assert_eq`.
            assert_eq!(1, 1);
            // The ONLY real call this file contributes: a same-file fn → Extracted.
            local_work();
        }
    }

    fn local_work() {}
}
