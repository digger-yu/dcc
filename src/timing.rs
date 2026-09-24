// src/timing.rs - Performance timing utilities
// Phase 4: Optimization tracking and performance measurement

/// Get current timestamp in nanoseconds
pub fn now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_nanos()
}

/// Measure execution time of a closure
pub fn measure<F, R>(f: F) -> (R, u128)
where
    F: FnOnce() -> R,
{
    let start = now();
    let result = f();
    let elapsed = now() - start;
    (result, elapsed)
}
