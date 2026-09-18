//! ARMFORTAS-owned OpenMP host runtime ABI.
//!
//! ABI version 1 uses a synchronous, fixed-signature parallel-region call:
//!
//! ```text
//! i32 afs_omp_parallel_region(entry, environment, if_value,
//!                             requested_threads, flags)
//! void entry(environment, thread_num, team_size)
//! ```
//!
//! `entry` and `environment` are borrowed for the duration of the call. The
//! encountering thread executes implicit task zero, worker threads execute the
//! remaining implicit tasks, and the function returns only after the team has
//! joined. `requested_threads <= 0` selects the current nthreads ICV. Version 1
//! reserves every flag bit; callers must pass zero.

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::time::Instant;

pub const AFS_OMP_ABI_VERSION: u32 = 1;
pub const AFS_OMP_SUCCESS: i32 = 0;
pub const AFS_OMP_ERROR_NULL_ENTRY: i32 = 1;
pub const AFS_OMP_ERROR_UNSUPPORTED_FLAGS: i32 = 2;
pub const AFS_OMP_ERROR_TEAM_PANIC: i32 = 3;

pub type AfsOmpRegionEntry = unsafe extern "C" fn(*mut c_void, i32, i32);

#[derive(Debug, Clone, Copy)]
struct TeamContext {
    thread_num: i32,
    team_size: i32,
    active: bool,
}

#[derive(Debug, Default)]
struct ThreadState {
    requested_threads: i32,
    teams: Vec<TeamContext>,
}

thread_local! {
    static THREAD_STATE: RefCell<ThreadState> = RefCell::new(ThreadState::default());
}

fn initial_thread_count() -> i32 {
    static INITIAL: OnceLock<i32> = OnceLock::new();
    *INITIAL.get_or_init(|| {
        std::env::var("OMP_NUM_THREADS")
            .ok()
            .and_then(|value| value.split(',').next()?.trim().parse::<i32>().ok())
            .filter(|&value| value > 0)
            .unwrap_or_else(num_processors)
    })
}

fn num_processors() -> i32 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(i32::MAX as usize) as i32)
        .unwrap_or(1)
        .max(1)
}

fn requested_thread_count() -> i32 {
    THREAD_STATE.with(|state| {
        let requested = state.borrow().requested_threads;
        if requested > 0 {
            requested
        } else {
            initial_thread_count()
        }
    })
}

struct TeamContextGuard {
    previous_requested_threads: i32,
}

impl Drop for TeamContextGuard {
    fn drop(&mut self) {
        THREAD_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.teams.pop();
            state.requested_threads = self.previous_requested_threads;
        });
    }
}

fn enter_team(context: TeamContext, inherited_requested_threads: i32) -> TeamContextGuard {
    THREAD_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let previous_requested_threads = state.requested_threads;
        state.requested_threads = inherited_requested_threads;
        state.teams.push(context);
        TeamContextGuard {
            previous_requested_threads,
        }
    })
}

fn run_implicit_task(
    entry: AfsOmpRegionEntry,
    environment: usize,
    thread_num: i32,
    team_size: i32,
    inherited_requested_threads: i32,
) {
    let _guard = enter_team(
        TeamContext {
            thread_num,
            team_size,
            active: team_size > 1,
        },
        inherited_requested_threads,
    );
    unsafe {
        entry(environment as *mut c_void, thread_num, team_size);
    }
}

#[no_mangle]
pub extern "C" fn afs_omp_abi_version() -> u32 {
    AFS_OMP_ABI_VERSION
}

/// Execute one implicit task per team member and synchronously join the team.
#[no_mangle]
pub extern "C" fn afs_omp_parallel_region(
    entry: Option<AfsOmpRegionEntry>,
    environment: *mut c_void,
    if_value: i32,
    requested_threads: i32,
    flags: u32,
) -> i32 {
    let Some(entry) = entry else {
        return AFS_OMP_ERROR_NULL_ENTRY;
    };
    if flags != 0 {
        return AFS_OMP_ERROR_UNSUPPORTED_FLAGS;
    }

    let inherited_requested_threads = requested_thread_count();
    let team_size = if if_value == 0 {
        1
    } else if requested_threads > 0 {
        requested_threads
    } else {
        inherited_requested_threads
    }
    .max(1);
    let environment = environment as usize;

    let result = catch_unwind(AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            for thread_num in 1..team_size {
                scope.spawn(move || {
                    run_implicit_task(
                        entry,
                        environment,
                        thread_num,
                        team_size,
                        inherited_requested_threads,
                    );
                });
            }
            run_implicit_task(
                entry,
                environment,
                0,
                team_size,
                inherited_requested_threads,
            );
        });
    }));

    if result.is_ok() {
        AFS_OMP_SUCCESS
    } else {
        AFS_OMP_ERROR_TEAM_PANIC
    }
}

#[no_mangle]
pub extern "C" fn afs_omp_get_thread_num() -> i32 {
    THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map_or(0, |team| team.thread_num)
    })
}

#[no_mangle]
pub extern "C" fn afs_omp_get_num_threads() -> i32 {
    THREAD_STATE.with(|state| state.borrow().teams.last().map_or(1, |team| team.team_size))
}

#[no_mangle]
pub extern "C" fn afs_omp_get_max_threads() -> i32 {
    requested_thread_count()
}

#[no_mangle]
pub extern "C" fn afs_omp_set_num_threads(count: i32) {
    if count > 0 {
        THREAD_STATE.with(|state| state.borrow_mut().requested_threads = count);
    }
}

#[no_mangle]
pub extern "C" fn afs_omp_get_num_procs() -> i32 {
    num_processors()
}

#[no_mangle]
pub extern "C" fn afs_omp_in_parallel() -> i32 {
    THREAD_STATE.with(|state| i32::from(state.borrow().teams.iter().any(|team| team.active)))
}

#[no_mangle]
pub extern "C" fn afs_omp_get_level() -> i32 {
    THREAD_STATE.with(|state| state.borrow().teams.len().min(i32::MAX as usize) as i32)
}

#[no_mangle]
pub extern "C" fn afs_omp_get_active_level() -> i32 {
    THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .iter()
            .filter(|team| team.active)
            .count()
            .min(i32::MAX as usize) as i32
    })
}

#[no_mangle]
pub extern "C" fn afs_omp_get_wtime() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[no_mangle]
pub extern "C" fn afs_omp_get_wtick() -> f64 {
    1.0e-9
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Observations {
        tasks: Mutex<Vec<(i32, i32, i32, i32)>>,
    }

    unsafe extern "C" fn record_task(environment: *mut c_void, thread_num: i32, team_size: i32) {
        let observations = unsafe { &*(environment as *const Observations) };
        observations.tasks.lock().unwrap().push((
            thread_num,
            team_size,
            afs_omp_in_parallel(),
            afs_omp_get_level(),
        ));
    }

    #[test]
    fn abi_version_and_initial_context_are_stable() {
        assert_eq!(afs_omp_abi_version(), 1);
        assert_eq!(afs_omp_get_thread_num(), 0);
        assert_eq!(afs_omp_get_num_threads(), 1);
        assert_eq!(afs_omp_in_parallel(), 0);
        assert_eq!(afs_omp_get_level(), 0);
        assert!(afs_omp_get_max_threads() >= 1);
        assert!(afs_omp_get_num_procs() >= 1);
    }

    #[test]
    fn parallel_region_runs_each_implicit_task_and_joins() {
        let observations = Observations::default();
        let status = afs_omp_parallel_region(
            Some(record_task),
            &observations as *const Observations as *mut c_void,
            1,
            4,
            0,
        );
        assert_eq!(status, AFS_OMP_SUCCESS);

        let mut tasks = observations.tasks.into_inner().unwrap();
        tasks.sort_unstable();
        assert_eq!(
            tasks,
            vec![(0, 4, 1, 1), (1, 4, 1, 1), (2, 4, 1, 1), (3, 4, 1, 1)]
        );
        assert_eq!(afs_omp_get_level(), 0, "team context leaked after join");
    }

    #[test]
    fn false_if_clause_uses_an_inactive_one_thread_team() {
        let observations = Observations::default();
        let status = afs_omp_parallel_region(
            Some(record_task),
            &observations as *const Observations as *mut c_void,
            0,
            8,
            0,
        );
        assert_eq!(status, AFS_OMP_SUCCESS);
        assert_eq!(observations.tasks.into_inner().unwrap(), vec![(0, 1, 0, 1)]);
    }

    #[test]
    fn explicit_thread_count_icv_is_used_and_restored() {
        afs_omp_set_num_threads(3);
        assert_eq!(afs_omp_get_max_threads(), 3);
        let observations = Observations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(record_task),
                &observations as *const Observations as *mut c_void,
                1,
                0,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(observations.tasks.into_inner().unwrap().len(), 3);
        assert_eq!(afs_omp_get_max_threads(), 3);
    }

    #[test]
    fn rejects_invalid_abi_inputs_without_invoking_user_code() {
        let observations = Observations::default();
        assert_eq!(
            afs_omp_parallel_region(None, std::ptr::null_mut(), 1, 1, 0),
            AFS_OMP_ERROR_NULL_ENTRY
        );
        assert_eq!(
            afs_omp_parallel_region(
                Some(record_task),
                &observations as *const Observations as *mut c_void,
                1,
                1,
                1,
            ),
            AFS_OMP_ERROR_UNSUPPORTED_FLAGS
        );
        assert!(observations.tasks.into_inner().unwrap().is_empty());
    }

    #[test]
    fn wall_clock_is_monotonic_and_reports_positive_resolution() {
        let first = afs_omp_get_wtime();
        let second = afs_omp_get_wtime();
        assert!(second >= first);
        assert!(afs_omp_get_wtick() > 0.0);
    }
}
