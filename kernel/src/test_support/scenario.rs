//! Declarative execution support for test-only kernel scenarios.
//!
//! Scenario bodies describe setup and assertions only. This module owns the
//! common `GTRT/1` lifecycle so a successful return always emits exactly one
//! `CASE_START` followed by one `PASS`, and suites terminate through one
//! `DONE PASS` record.

use super::protocol;

/// Result returned by one kernel scenario or a fallible scenario helper.
///
/// `Err` carries a stable enum-like protocol reason. Returning it from the
/// coordinator path emits `FAIL` for the currently executing descriptor;
/// successful scenario bodies return `Ok(())` and are reported as `PASS`.
pub(crate) type ScenarioResult<T = ()> = Result<T, &'static str>;

/// One named, sequential kernel contract scenario.
///
/// The stable name is part of the test protocol contract. The body runs in the
/// suite coordinator thread and may coordinate bounded work on other CPUs.
/// Scenario descriptors contain only static data and do not allocate.
pub(crate) struct Scenario {
    name: &'static str,
    body: fn() -> ScenarioResult,
}

impl Scenario {
    /// Construct a static scenario descriptor.
    ///
    /// # Arguments
    ///
    /// * `name` - Stable machine-readable case identifier expected by the host
    ///   QEMU case definition.
    /// * `body` - Scenario function containing setup and assertions. `Ok(())`
    ///   means success and `Err(reason)` means failure.
    ///
    /// # Returns
    ///
    /// Returns an allocation-free descriptor suitable for static suite tables.
    pub(crate) const fn new(name: &'static str, body: fn() -> ScenarioResult) -> Self {
        Self { name, body }
    }

    fn run(&self) {
        protocol::case_start(self.name);
        if let Err(reason) = (self.body)() {
            protocol::fail(self.name, reason);
        }
        protocol::pass(self.name);
    }
}

/// Execute a finite scenario table as one kernel test suite.
///
/// The function emits suite readiness, runs scenarios in declaration order,
/// wraps every body in the common case lifecycle, and emits terminal success.
/// Protocol output and iteration are allocation-free; individual scenario
/// bodies retain responsibility for their own bounded behavior.
///
/// # Arguments
///
/// * `suite` - Stable machine-readable suite identifier.
/// * `scenarios` - Static ordered table of scenarios expected by the host case.
///
/// # Returns
///
/// This function never returns after emitting `DONE PASS` for the suite.
pub(crate) fn run_suite(suite: &'static str, scenarios: &'static [Scenario]) -> ! {
    protocol::ready(suite);
    for scenario in scenarios {
        scenario.run();
    }
    protocol::done(suite)
}

/// Declare a single-threaded kernel test suite and its named scenarios.
///
/// The generated static descriptor table preserves declaration order, while
/// the generated coordinator and thread set delegate all protocol lifecycle
/// records to [`run_suite`]. Scenario functions remain ordinary, independently
/// documented Rust functions returning [`ScenarioResult`], so `rustfmt` and
/// rustdoc can process their bodies normally.
///
/// ```ignore
/// kernel_test_suite! {
///     suite: SUITE,
///     threads: THREADS,
///     scenarios: [
///         "stable-case-id" => example,
///     ],
/// }
///
/// /// Explain the behavior and invariant under test.
/// fn example() -> ScenarioResult {
///     if !condition() {
///         return Err("CONDITION");
///     }
///     Ok(())
/// }
/// ```
macro_rules! kernel_test_suite {
    (
        suite: $suite:expr,
        threads: $threads:ident,
        scenarios: [
            $(
                $name:literal => $scenario:path
            ),+ $(,)?
        ],
    ) => {
        const SCENARIOS: &[$crate::test_support::scenario::Scenario] = &[
            $(
                $crate::test_support::scenario::Scenario::new($name, $scenario),
            )+
        ];

        /// Static coordinator thread selected by this QEMU test feature.
        pub(crate) const $threads: [$crate::sched::StaticThread; 1] = [
            $crate::sched::StaticThread::new(
                coordinator,
                $crate::sched::ThreadArg::empty(),
            ),
        ];

        fn coordinator(_arg: $crate::sched::ThreadArg) -> usize {
            $crate::test_support::scenario::run_suite($suite, SCENARIOS)
        }
    };
}

pub(crate) use kernel_test_suite;
