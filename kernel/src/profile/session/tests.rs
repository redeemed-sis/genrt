//! Pure session-state and export tests; no global scheduler initialization.
use super::*;
use crate::profile::format::{BUDGET, FOCUSED};

fn owner() -> ThreadId {
    ThreadId::new(7, 4)
}
fn cpu0() -> CpuId {
    CpuId::from_index(0).unwrap()
}
fn config(capacity: u32) -> Config {
    Config {
        mode: FULL,
        max_records: capacity,
        start_phase: 0,
        end_phase: 13,
    }
}
fn session(config: Config) -> Session {
    Session::allocate(owner(), cpu0(), config, |v, n| {
        v.try_reserve_exact(n).map_err(|_| errno::ENOMEM)
    })
    .unwrap()
}
fn token(thread: ThreadId, cpu: CpuId, sequence: u64) -> WaitToken {
    WaitToken::for_profile_test(thread, cpu, sequence)
}

#[test]
fn fixed_layout_and_budget_are_exact() {
    assert_eq!(core::mem::size_of::<Config>(), 16);
    assert_eq!(core::mem::size_of::<Header>(), 104);
    assert_eq!(core::mem::size_of::<Record>(), 160);
    let max = ((BUDGET - 104) / 160) as u32;
    assert_eq!(max, 26_213);
    assert!(config(max).bytes().unwrap() <= BUDGET);
    assert_eq!(config(max + 1).bytes(), Err(errno::EINVAL));
    assert!(config(10_000).bytes().is_ok());
    for capacity in [0, u32::MAX] {
        assert_eq!(config(capacity).bytes(), Err(errno::EINVAL));
    }
    for mode in [0, 3, u32::MAX] {
        assert_eq!(Config { mode, ..config(1) }.bytes(), Err(errno::EINVAL));
    }
    for (start_phase, end_phase) in [(0, 0), (3, 2), (0, 14), (u32::MAX, 1)] {
        assert_eq!(
            Config {
                start_phase,
                end_phase,
                ..config(1)
            }
            .bytes(),
            Err(errno::EINVAL)
        );
    }
}

#[test]
fn allocation_failure_is_returned_before_publication() {
    let result = Session::allocate(owner(), cpu0(), config(1), |_, _| Err(errno::ENOMEM));
    assert!(matches!(result, Err(errno::ENOMEM)));
    let result = Session::allocate(owner(), cpu0(), config(0), |_, _| {
        panic!("invalid config reserved")
    });
    assert!(matches!(result, Err(errno::EINVAL)));
    let s = session(config(2));
    assert_eq!(s.records.len(), 2);
    assert_eq!(s.records.capacity(), 2);
    assert!(s.records.iter().all(|r| bytes(r).iter().all(|b| *b == 0)));
}

#[test]
fn exact_generation_cpu_sequence_and_deadline_are_required() {
    let mut s = session(config(1));
    let good = token(owner(), cpu0(), 3);
    s.begin(owner(), 100, false, 1);
    for wrong in [
        token(ThreadId::new(7, 5), cpu0(), 3),
        token(ThreadId::new(8, 4), cpu0(), 3),
        token(owner(), CpuId::from_index(1).unwrap(), 3),
    ] {
        s.bind(wrong, 100);
        assert!(s.cycle.as_ref().unwrap().token.is_none());
    }
    s.bind(good, 101);
    assert!(s.cycle.as_ref().unwrap().token.is_none());
    s.bind(good, 100);
    assert_eq!(s.records[0].wait_sequence, 3);
    let before = s.records[0].phase_present;
    for (t, deadline) in [(token(owner(), cpu0(), 4), 100), (good, 99)] {
        assert_eq!(s.expired(t, deadline, 20, Some(21)), None);
        assert_eq!(s.records[0].phase_present, before);
    }
    assert_eq!(s.expired(good, 100, 30, Some(31)), Some(0));
    assert_eq!(s.expired(good, 100, 80, Some(81)), None);
    assert_eq!(s.records[0].counters[Phase::IrqEntry as usize], 30);
    assert_eq!(s.records[0].counters[Phase::Acknowledge as usize], 31);
}

#[test]
fn focused_mode_has_exactly_two_observations_and_no_unselected_reads() {
    for start in 0..13 {
        for end in start + 1..14 {
            let mut s = session(Config {
                mode: FOCUSED,
                start_phase: start,
                end_phase: end,
                ..config(1)
            });
            let before = crate::test_arch_stubs::profile_counter_reads();
            let phases = [
                Phase::SvcEntry,
                Phase::DeadlineValidated,
                Phase::TimerPublished,
                Phase::WaitCommitted,
                Phase::SleepHandoff,
                Phase::IrqEntry,
                Phase::Acknowledge,
                Phase::ExpiredExtracted,
                Phase::SchedulerAcquired,
                Phase::WakeSelected,
                Phase::ContextRestored,
                Phase::TimerRearmed,
                Phase::InterruptCompleted,
                Phase::ReturnToUser,
            ];
            for phase in phases {
                s.stamp(0, phase, None);
            }
            assert_eq!(crate::test_arch_stubs::profile_counter_reads() - before, 2);
            assert_eq!(s.records[0].phase_present, (1 << start) | (1 << end));
            for (id, value) in s.records[0].counters.iter().enumerate() {
                assert_eq!(*value != 0, id == start as usize || id == end as usize);
            }
            for phase in phases {
                s.stamp(0, phase, None);
            }
            assert_eq!(crate::test_arch_stubs::profile_counter_reads() - before, 2);
        }
    }
}

#[test]
fn generic_deferred_resume_retains_original_event_irq_and_wait_identity() {
    let mut s = session(config(2));
    let good = token(owner(), cpu0(), 10);
    s.begin(owner(), 100, false, 1);
    s.bind(good, 100);
    assert_eq!(s.expired(good, 100, 50, Some(51)), Some(0));
    s.resume(ThreadId::new(8, 4), Phase::WakeSelected, false);
    s.returned(ThreadId::new(8, 4));
    assert_eq!(s.completed, 0);
    assert_eq!(
        s.records[0].phase_present & (1 << Phase::WakeSelected as u32),
        0
    );
    // Later timeslice/IPI supplies eventual target handoff, not expiry probes.
    assert_eq!(s.expired(good, 100, 500, Some(501)), None);
    s.resume(owner(), Phase::WakeSelected, false);
    s.resume(owner(), Phase::ContextRestored, false);
    assert_eq!(s.records[0].wake_path, GENERIC);
    assert_eq!(s.records[0].counters[5], 50);
    assert_eq!(s.records[0].counters[6], 51);
    assert_eq!(s.records[0].wait_sequence, 10);
    for phase in [
        Phase::TimerPublished,
        Phase::WaitCommitted,
        Phase::SleepHandoff,
        Phase::SchedulerAcquired,
        Phase::TimerRearmed,
        Phase::InterruptCompleted,
    ] {
        s.stamp(0, phase, None);
    }
    s.records[0].programmed_deadline = 100;
    s.returned(owner());
    s.returned(owner());
    assert_eq!(s.completed, 1);
    s.freeze();
    assert_eq!(s.header.incomplete, 0);
}

#[test]
fn capacity_keeps_records_and_counts_lost_incomplete_and_early() {
    let mut s = session(config(2));
    s.begin(ThreadId::new(7, 5), 10, true, 1);
    assert_eq!(s.header.recorded, 0);
    s.begin(owner(), 10, true, 1);
    s.returned(owner());
    assert_eq!(s.records[0].wake_path, EARLY);
    assert_eq!(s.records[0].wait_sequence, 0);
    assert_eq!(s.records[0].programmed_deadline, 0);
    assert_eq!(s.records[0].phase_present, (1 << 0) | (1 << 1) | (1 << 13));
    s.begin(owner(), 20, false, 2);
    let saved0 = bytes(&s.records[0]).to_vec();
    let saved1 = bytes(&s.records[1]).to_vec();
    for _ in 0..5 {
        s.begin(owner(), 30, false, 3);
    }
    assert_eq!(bytes(&s.records[0]), saved0);
    assert_eq!(bytes(&s.records[1]), saved1);
    assert_eq!(s.header.recorded, 2);
    assert_eq!(s.header.lost, 5);
    assert_eq!(s.header.early, 1);
    s.freeze();
    assert_eq!(s.header.incomplete, 1);
    s.begin(owner(), 40, true, 4);
    s.stamp(0, Phase::IrqEntry, Some(100));
    assert_eq!(s.header.lost, 5);
    assert_eq!(bytes(&s.records[0]), saved0);
}

#[test]
fn frozen_export_chunk_boundaries_eof_negative_and_stability() {
    let mut s = session(config(3));
    let mut chunk = [0u8; 71];
    assert_eq!(s.read(owner(), 0, &mut chunk), Err(errno::EBUSY));
    assert_eq!(
        s.read(ThreadId::new(7, 5), 0, &mut chunk),
        Err(errno::EPERM)
    );
    s.begin(owner(), 100, true, 1);
    s.returned(owner());
    s.begin(owner(), 200, true, 2);
    s.returned(owner());
    s.freeze();
    let mut expected = bytes(&s.header).to_vec();
    expected.extend_from_slice(bytes(&s.records[0]));
    expected.extend_from_slice(bytes(&s.records[1]));
    assert_eq!(expected.len(), 104 + 2 * 160);
    let mut result = Vec::new();
    loop {
        let len = s.read(owner(), result.len(), &mut chunk).unwrap();
        if len == 0 {
            break;
        }
        result.extend_from_slice(&chunk[..len]);
    }
    assert_eq!(result, expected);
    for offset in 0..expected.len() {
        let len = s.read(owner(), offset, &mut chunk).unwrap();
        assert_eq!(&chunk[..len], &expected[offset..offset + len]);
    }
    assert_eq!(s.read(owner(), usize::MAX, &mut chunk), Ok(0));
    assert_eq!(s.read(owner(), 0, &mut []), Ok(0));
    s.freeze();
    assert_eq!(bytes(&s.header), &expected[..104]);
}

#[test]
fn incomplete_uses_mode_path_presence_and_effective_programming() {
    let mut s = session(config(2));
    s.begin(owner(), 100, false, 1);
    let good = token(owner(), cpu0(), 1);
    s.bind(good, 100);
    s.expired(good, 100, 20, Some(21));
    s.resume(owner(), Phase::ContextRestored, false);
    s.returned(owner());
    assert_eq!(s.completed, 0);
    s.freeze();
    // Return alone does not make a partial full trace complete.
    assert_eq!(s.header.incomplete, 1);

    let mut s = session(Config {
        mode: FOCUSED,
        start_phase: 0,
        end_phase: 1,
        ..config(2)
    });
    s.begin(owner(), 100, false, 1);
    // The pair is present, but this blocked cycle has no actual CVAL evidence.
    s.freeze();
    assert_eq!(s.header.incomplete, 1);

    let mut s = session(Config {
        mode: FOCUSED,
        start_phase: 5,
        end_phase: 13,
        ..config(2)
    });
    s.begin(owner(), 100, true, 1);
    s.returned(owner());
    s.freeze();
    // Early paths do not require impossible IRQ observations.
    assert_eq!(s.records[0].phase_present, 1 << 13);
    assert_eq!(s.header.incomplete, 0);
}

#[test]
fn optimized_idle_resume_and_new_wait_reject_stale_expiry() {
    let mut s = session(config(2));
    let first = token(owner(), cpu0(), 1);
    let second = token(owner(), cpu0(), 2);
    s.begin(owner(), 100, false, 1);
    s.bind(first, 100);
    s.expired(first, 100, 10, Some(11));
    s.resume(owner(), Phase::WakeSelected, true);
    s.resume(owner(), Phase::ContextRestored, false);
    s.returned(owner());
    assert_eq!(s.records[0].wake_path, IDLE);
    s.begin(owner(), 200, false, 2);
    s.bind(second, 200);
    assert_eq!(s.expired(first, 100, 80, Some(81)), None);
    assert_eq!(s.records[1].phase_present & (1 << 5), 0);
    assert_eq!(s.records[0].ordinal, 0);
    assert_eq!(s.records[1].ordinal, 1);
}
