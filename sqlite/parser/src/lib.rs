pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod token;

pub use parser::MAX_EXPR_DEPTH;

/// Stack that must remain before parsing or compiling a statement goes one level deeper. Both
/// recurse once per nested subquery, CTE, compound SELECT term and expression, and a thread
/// that runs out of stack aborts the whole process; below this margin the statement is refused
/// instead. The margin covers the deepest single step between two checks, in debug builds too.
pub const STACK_MARGIN: usize = 512 * 1024;

/// The calling thread has less than [`STACK_MARGIN`] stack left (MySQL refuses such a
/// statement the same way, "thread stack overrun"). Always false where the stack size is
/// unknown (WebAssembly).
pub fn stack_exhausted() -> bool {
    stacker::remaining_stack().is_some_and(|left| left < STACK_MARGIN)
}

type Result<T, E = error::Error> = std::result::Result<T, E>;
