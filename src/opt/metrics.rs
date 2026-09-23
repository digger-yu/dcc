// src/opt/metrics.rs - Optimization Metrics and Statistics Tracking
// Phase 4: Performance Optimization & Tuning
//
// First Principle: Measure everything to optimize effectively
// Strategy: Collect detailed metrics at each optimization stage

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Unified optimization metrics tracker
#[derive(Debug, Default)]
pub struct OptimizationMetrics {
    /// Timing statistics
    pub timings: HashMap<String, Duration>,
    
    /// Count-based statistics
    pub counters: HashMap<String, usize>,
    
    /// Size-related metrics
    pub size_before: usize,
    pub size_after: usize,
    
    /// Performance deltas
    pub perf_delta: HashMap<String, f64>,
}

impl OptimizationMetrics {

    /// Record timing for an optimization phase
    pub fn record_timing(&mut self, name: &str, duration: Duration) {
        self.timings.insert(name.to_string(), duration);
    }

    /// Record a counter increment
    pub fn increment(&mut self, name: &str, count: usize) {
        *self.counters.entry(name.to_string()).or_insert(0) += count;
    }

    /// Record size before/after comparison
    pub fn record_size_change(&mut self, before: usize, after: usize) {
        self.size_before = before;
        self.size_after = after;
        
        if before > 0 {
            let delta = ((after as f64 / before as f64) - 1.0) * 100.0;
            self.perf_delta.insert("size".to_string(), delta);
        }
    }

    /// Record performance improvement percentage
    pub fn record_improvement(&mut self, metric: &str, improvement_pct: f64) {
        self.perf_delta.insert(metric.to_string(), improvement_pct);
    }

    /// Get time taken for a specific optimization
    pub fn get_time(&self, name: &str) -> Option<&Duration> {
        self.timings.get(name)
    }

    /// Get count for a specific metric
    pub fn get_count(&self, name: &str) -> Option<usize> {
        self.counters.get(name).copied()
    }

    /// Get total number of optimizations applied
    pub fn total_optimizations(&self) -> usize {
        self.counters.values().sum()
    }

    /// Check if size increased more than acceptable threshold
    pub fn size_within_limits(&self, max_increase_pct: f64) -> bool {
        if self.size_before == 0 {
            return true;
        }
        let increase = ((self.size_after as f64 / self.size_before as f64) - 1.0) * 100.0;
        increase <= max_increase_pct
    }

    /// Format metrics as human-readable string
    pub fn format_report(&self) -> String {
        let mut report = String::from("=== Optimization Metrics Report ===\n\n");
        
        // Timings section
        report.push_str("Timing (ms):\n");
        for (name, duration) in &self.timings {
            let ms = duration.as_millis();
            report.push_str(&format!("  {:30}: {:6} ms\n", name, ms));
        }
        report.push('\n');
        
        // Counters section
        report.push_str("Optimization Counts:\n");
        for (name, count) in &self.counters {
            report.push_str(&format!("  {:30}: {}\n", name, count));
        }
        report.push('\n');
        
        // Size changes
        report.push_str("Size Impact:\n");
        if self.size_before > 0 {
            let pct = ((self.size_after as f64 / self.size_before as f64) - 1.0) * 100.0;
            let sign = if pct >= 0.0 { "+" } else { "" };
            report.push_str(&format!(
                "  {:30}: {}{:,.2}%\n", 
                "Total", 
                sign, pct
            ));
            report.push_str(&format!(
                "  {:30}: {}, {}\n",
                "Before",
                Self::format_bytes(self.size_before),
                Self::format_bytes(self.size_after)
            ));
        }
        report.push('\n');
        
        // Performance improvements
        if !self.perf_delta.is_empty() {
            report.push_str("Performance Improvements:\n");
            for (metric, improvement) in &self.perf_delta {
                let sign = if *improvement >= 0.0 { "+" } else { "" };
                report.push_str(&format!("  {:30}: {:+6.2}%\n", metric, improvement));
            }
            report.push('\n');
        }
        
        report
    }

    /// Format bytes as human-readable
    fn format_bytes(bytes: usize) -> String {
        const KB: usize = 1024;
        const MB: usize = 1024 * 1024;
        
        if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }

    /// Create a scope timer for automatic timing measurement
    pub fn timer<'a>(&'a self, name: &'static str) -> ScopeTimer<'a> {
        ScopeTimer::new(name)
    }
}

/// Automatic timing scope guard
pub struct ScopeTimer<'a> {
    start: Instant,
    name: &'static str,
    metrics: &'a OptimizationMetrics,
}

impl<'a> ScopeTimer<'a> {
    pub fn new(name: &'static str) -> Self {
        Self {
            start: Instant::now(),
            name,
            metrics: &OptimizationMetrics::default(),
        }
    }
}

impl<'a> Drop for ScopeTimer<'a> {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        // Note: In real usage, would track against actual metrics instance
        eprintln!("{} completed in {}", self.name, elapsed.as_millis());
    }
}

/// Builder pattern for creating benchmark scenarios
pub struct BenchmarkBuilder {
    name: String,
    inputs: Vec<BenchmarkInput>,
}

#[derive(Debug)]
pub struct BenchmarkInput {
    pub name: String,
    pub code_size: usize,
    pub expected_speedup: Option<f64>,
}

impl BenchmarkBuilder {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            inputs: Vec::new(),
        }
    }

    pub fn add_input(mut self, name: &str, code_size: usize) -> Self {
        self.inputs.push(BenchmarkInput {
            name: name.to_string(),
            code_size,
            expected_speedup: None,
        });
        self
    }

    pub fn run_benchmarks(self) -> Vec<BenchmarkResult> {
        // Placeholder - would actually compile and measure
        vec![]
    }
}

#[derive(Debug)]
pub struct BenchmarkResult {
    pub input_name: String,
    pub compile_time_ms: u128,
    pub binary_size: usize,
    pub speedup_factor: f64,
}
