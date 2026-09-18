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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub const AFS_OMP_ABI_VERSION: u32 = 1;
pub const AFS_OMP_SUCCESS: i32 = 0;
pub const AFS_OMP_ERROR_NULL_ENTRY: i32 = 1;
pub const AFS_OMP_ERROR_UNSUPPORTED_FLAGS: i32 = 2;
pub const AFS_OMP_ERROR_TEAM_PANIC: i32 = 3;

pub type AfsOmpRegionEntry = unsafe extern "C" fn(*mut c_void, i32, i32);

const SUPPORTED_ACTIVE_LEVELS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleKind {
    Static,
    Dynamic,
    Guided,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleModifier {
    Unspecified,
    Monotonic,
    Nonmonotonic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunSchedule {
    kind: ScheduleKind,
    modifier: ScheduleModifier,
    chunk: Option<i32>,
}

impl Default for RunSchedule {
    fn default() -> Self {
        Self {
            kind: ScheduleKind::Static,
            modifier: ScheduleModifier::Unspecified,
            chunk: None,
        }
    }
}

/// Process-wide initial ICV values.
///
/// Environment variables are read once, at the first OpenMP runtime call.
/// Invalid values use the documented ARMFORTAS defaults: available processors
/// for `OMP_NUM_THREADS`, false dynamic adjustment, no practical thread limit,
/// one active level, and an implementation-defined static schedule. A valid
/// multi-value `OMP_NUM_THREADS` list enables every supported active level
/// unless `OMP_MAX_ACTIVE_LEVELS` overrides it. Per-task
/// `omp_set_num_threads` state overrides the corresponding environment entry;
/// an explicit `num_threads` clause in the compiler/runtime ABI overrides both.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeConfig {
    nthreads: Vec<i32>,
    dynamic: bool,
    thread_limit: i32,
    max_active_levels: usize,
    #[allow(dead_code)] // Consumed when runtime-scheduled worksharing lands.
    schedule: RunSchedule,
    num_processors: i32,
}

impl RuntimeConfig {
    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>, num_processors: i32) -> Self {
        let num_processors = num_processors.max(1);
        let nthreads = lookup("OMP_NUM_THREADS")
            .as_deref()
            .and_then(parse_positive_integer_list)
            .unwrap_or_else(|| vec![num_processors]);
        let dynamic = lookup("OMP_DYNAMIC")
            .as_deref()
            .and_then(parse_openmp_bool)
            .unwrap_or(false);
        let thread_limit = lookup("OMP_THREAD_LIMIT")
            .as_deref()
            .and_then(parse_positive_integer)
            .unwrap_or(i32::MAX);
        let default_max_active_levels = if nthreads.len() > 1 {
            SUPPORTED_ACTIVE_LEVELS
        } else {
            1
        };
        let max_active_levels = lookup("OMP_MAX_ACTIVE_LEVELS")
            .as_deref()
            .and_then(parse_nonnegative_integer)
            .map(|levels| levels.min(SUPPORTED_ACTIVE_LEVELS))
            .unwrap_or(default_max_active_levels);
        let schedule = lookup("OMP_SCHEDULE")
            .as_deref()
            .and_then(parse_schedule)
            .unwrap_or_default();

        Self {
            nthreads,
            dynamic,
            thread_limit,
            max_active_levels,
            schedule,
            num_processors,
        }
    }
}

fn parse_positive_integer(value: &str) -> Option<i32> {
    value.trim().parse::<i32>().ok().filter(|value| *value > 0)
}

fn parse_nonnegative_integer(value: &str) -> Option<usize> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .and_then(|value| usize::try_from(value).ok())
}

fn parse_positive_integer_list(value: &str) -> Option<Vec<i32>> {
    let parsed: Option<Vec<_>> = value.split(',').map(parse_positive_integer).collect();
    parsed.filter(|values| !values.is_empty())
}

fn parse_openmp_bool(value: &str) -> Option<bool> {
    if value.trim().eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.trim().eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_schedule(value: &str) -> Option<RunSchedule> {
    let mut colon_parts = value.split(':');
    let first = colon_parts.next()?.trim();
    let second = colon_parts.next().map(str::trim);
    if colon_parts.next().is_some() {
        return None;
    }
    let (modifier, schedule) = match second {
        Some(schedule) => {
            let modifier = if first.eq_ignore_ascii_case("monotonic") {
                ScheduleModifier::Monotonic
            } else if first.eq_ignore_ascii_case("nonmonotonic") {
                ScheduleModifier::Nonmonotonic
            } else {
                return None;
            };
            (modifier, schedule)
        }
        None => (ScheduleModifier::Unspecified, first),
    };

    let mut schedule_parts = schedule.split(',');
    let kind = match schedule_parts.next()?.trim().to_ascii_lowercase().as_str() {
        "static" => ScheduleKind::Static,
        "dynamic" => ScheduleKind::Dynamic,
        "guided" => ScheduleKind::Guided,
        "auto" => ScheduleKind::Auto,
        _ => return None,
    };
    let chunk = match schedule_parts.next() {
        Some(value) => Some(parse_positive_integer(value)?),
        None => None,
    };
    if schedule_parts.next().is_some() {
        return None;
    }
    Some(RunSchedule {
        kind,
        modifier,
        chunk,
    })
}

fn runtime_config() -> &'static RuntimeConfig {
    static CONFIG: OnceLock<RuntimeConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        RuntimeConfig::from_lookup(|name| std::env::var(name).ok(), num_processors())
    })
}

#[derive(Debug)]
struct ContentionGroup {
    limit: usize,
    active_threads: AtomicUsize,
}

impl ContentionGroup {
    fn new(limit: i32) -> Self {
        Self {
            limit: usize::try_from(limit.max(1)).unwrap_or(usize::MAX),
            active_threads: AtomicUsize::new(1),
        }
    }

    fn reserve_additional(&self, requested: usize) -> usize {
        let mut current = self.active_threads.load(Ordering::Acquire);
        loop {
            let granted = requested.min(self.limit.saturating_sub(current));
            if granted == 0 {
                return 0;
            }
            match self.active_threads.compare_exchange_weak(
                current,
                current + granted,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return granted,
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, count: usize) {
        if count > 0 {
            self.active_threads.fetch_sub(count, Ordering::AcqRel);
        }
    }
}

#[derive(Debug, Clone)]
struct TeamContext {
    thread_num: i32,
    team_size: i32,
    active: bool,
    contention_group: Arc<ContentionGroup>,
}

#[derive(Debug, Default)]
struct ThreadState {
    requested_threads: i32,
    teams: Vec<TeamContext>,
}

thread_local! {
    static THREAD_STATE: RefCell<ThreadState> = RefCell::new(ThreadState::default());
}

fn num_processors() -> i32 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(i32::MAX as usize) as i32)
        .unwrap_or(1)
        .max(1)
}

fn requested_thread_count(config: &RuntimeConfig) -> i32 {
    THREAD_STATE.with(|state| {
        let state = state.borrow();
        let requested = state.requested_threads;
        if requested > 0 {
            requested
        } else {
            let level = state.teams.len();
            config
                .nthreads
                .get(level)
                .or_else(|| config.nthreads.last())
                .copied()
                .unwrap_or(config.num_processors)
        }
    })
}

fn requested_thread_override() -> i32 {
    THREAD_STATE.with(|state| state.borrow().requested_threads)
}

fn active_level() -> usize {
    THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .iter()
            .filter(|team| team.active)
            .count()
    })
}

fn current_contention_group() -> Option<Arc<ContentionGroup>> {
    THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.contention_group))
    })
}

fn determine_team_size(
    config: &RuntimeConfig,
    if_value: i32,
    requested_threads: i32,
    fallback_threads: i32,
    parent_active_level: usize,
) -> i32 {
    if if_value == 0 || parent_active_level >= config.max_active_levels {
        return 1;
    }
    let mut team_size = if requested_threads > 0 {
        requested_threads
    } else {
        fallback_threads
    }
    .max(1)
    .min(config.thread_limit);
    if config.dynamic {
        team_size = team_size.min(config.num_processors);
    }
    team_size
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
    contention_group: Arc<ContentionGroup>,
) {
    let _guard = enter_team(
        TeamContext {
            thread_num,
            team_size,
            active: team_size > 1,
            contention_group,
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

    let config = runtime_config();
    let inherited_requested_threads = requested_thread_override();
    let fallback_threads = requested_thread_count(config);
    let requested_team_size = determine_team_size(
        config,
        if_value,
        requested_threads,
        fallback_threads,
        active_level(),
    );
    let contention_group = current_contention_group()
        .unwrap_or_else(|| Arc::new(ContentionGroup::new(config.thread_limit)));
    let reserved_threads = contention_group.reserve_additional(
        usize::try_from(requested_team_size.saturating_sub(1)).unwrap_or(usize::MAX),
    );
    let team_size = i32::try_from(reserved_threads.saturating_add(1)).unwrap_or(i32::MAX);
    let environment = environment as usize;

    let result = catch_unwind(AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            for thread_num in 1..team_size {
                let contention_group = Arc::clone(&contention_group);
                scope.spawn(move || {
                    run_implicit_task(
                        entry,
                        environment,
                        thread_num,
                        team_size,
                        inherited_requested_threads,
                        contention_group,
                    );
                });
            }
            run_implicit_task(
                entry,
                environment,
                0,
                team_size,
                inherited_requested_threads,
                Arc::clone(&contention_group),
            );
        });
    }));
    contention_group.release(reserved_threads);

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
    requested_thread_count(runtime_config())
}

#[no_mangle]
pub extern "C" fn afs_omp_set_num_threads(count: i32) {
    if count > 0 {
        THREAD_STATE.with(|state| state.borrow_mut().requested_threads = count);
    }
}

#[no_mangle]
pub extern "C" fn afs_omp_get_num_procs() -> i32 {
    runtime_config().num_processors
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn reset_thread_state() {
        THREAD_STATE.with(|state| *state.borrow_mut() = ThreadState::default());
    }

    fn config(entries: &[(&str, &str)], processors: i32) -> RuntimeConfig {
        let entries: HashMap<_, _> = entries.iter().copied().collect();
        RuntimeConfig::from_lookup(
            |name| entries.get(name).map(|value| (*value).to_string()),
            processors,
        )
    }

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
        reset_thread_state();
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
        reset_thread_state();
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
        reset_thread_state();
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
        reset_thread_state();
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
        reset_thread_state();
    }

    #[test]
    fn rejects_invalid_abi_inputs_without_invoking_user_code() {
        reset_thread_state();
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

    #[test]
    fn parses_openmp_environment_icvs_and_precedence() {
        let parsed = config(
            &[
                ("OMP_NUM_THREADS", " 4, 3,2 "),
                ("OMP_DYNAMIC", "TrUe"),
                ("OMP_THREAD_LIMIT", "7"),
                ("OMP_MAX_ACTIVE_LEVELS", "2"),
                ("OMP_SCHEDULE", "monotonic:guided, 5"),
            ],
            12,
        );
        assert_eq!(parsed.nthreads, vec![4, 3, 2]);
        assert!(parsed.dynamic);
        assert_eq!(parsed.thread_limit, 7);
        assert_eq!(parsed.max_active_levels, 2);
        assert_eq!(
            parsed.schedule,
            RunSchedule {
                kind: ScheduleKind::Guided,
                modifier: ScheduleModifier::Monotonic,
                chunk: Some(5),
            }
        );

        let list_default = config(&[("OMP_NUM_THREADS", "4,3")], 12);
        assert_eq!(list_default.max_active_levels, SUPPORTED_ACTIVE_LEVELS);
    }

    #[test]
    fn invalid_openmp_environment_values_use_documented_defaults() {
        let parsed = config(
            &[
                ("OMP_NUM_THREADS", "4,0,2"),
                ("OMP_DYNAMIC", "sometimes"),
                ("OMP_THREAD_LIMIT", "-1"),
                ("OMP_MAX_ACTIVE_LEVELS", "-2"),
                ("OMP_SCHEDULE", "sideways,0"),
            ],
            6,
        );
        assert_eq!(parsed.nthreads, vec![6]);
        assert!(!parsed.dynamic);
        assert_eq!(parsed.thread_limit, i32::MAX);
        assert_eq!(parsed.max_active_levels, 1);
        assert_eq!(parsed.schedule, RunSchedule::default());
    }

    #[test]
    fn team_selection_honors_clause_setter_dynamic_limit_and_nesting_precedence() {
        reset_thread_state();
        let mut parsed = config(&[("OMP_NUM_THREADS", "8"), ("OMP_THREAD_LIMIT", "5")], 4);
        assert_eq!(requested_thread_count(&parsed), 8);
        afs_omp_set_num_threads(3);
        assert_eq!(requested_thread_count(&parsed), 3);
        assert_eq!(determine_team_size(&parsed, 1, 2, 3, 0), 2);
        assert_eq!(determine_team_size(&parsed, 1, 0, 3, 0), 3);

        parsed.dynamic = true;
        assert_eq!(determine_team_size(&parsed, 1, 0, 8, 0), 4);
        parsed.dynamic = false;
        assert_eq!(determine_team_size(&parsed, 1, 0, 8, 0), 5);
        assert_eq!(determine_team_size(&parsed, 0, 4, 4, 0), 1);
        assert_eq!(determine_team_size(&parsed, 1, 4, 4, 1), 1);
        reset_thread_state();
    }

    #[test]
    fn contention_group_reservations_never_exceed_thread_limit() {
        let group = ContentionGroup::new(4);
        assert_eq!(group.reserve_additional(3), 3);
        assert_eq!(group.reserve_additional(3), 0);
        group.release(2);
        assert_eq!(group.reserve_additional(3), 2);
        group.release(3);
        assert_eq!(group.active_threads.load(Ordering::Acquire), 1);
    }

    #[derive(Default)]
    struct NestedObservations {
        states: Mutex<Vec<(i32, i32, i32, i32, i32)>>,
    }

    unsafe extern "C" fn nested_inactive_task(
        environment: *mut c_void,
        thread_num: i32,
        team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const NestedObservations) };
        observations.states.lock().unwrap().push((
            1,
            thread_num,
            team_size,
            afs_omp_get_level(),
            afs_omp_get_active_level(),
        ));
    }

    unsafe extern "C" fn outer_task_with_nested_inactive_region(
        environment: *mut c_void,
        thread_num: i32,
        team_size: i32,
    ) {
        if thread_num != 0 {
            return;
        }
        let observations = unsafe { &*(environment as *const NestedObservations) };
        observations.states.lock().unwrap().push((
            0,
            thread_num,
            team_size,
            afs_omp_get_level(),
            afs_omp_get_active_level(),
        ));
        assert_eq!(
            afs_omp_parallel_region(Some(nested_inactive_task), environment, 0, 4, 0),
            AFS_OMP_SUCCESS
        );
        observations.states.lock().unwrap().push((
            2,
            afs_omp_get_thread_num(),
            afs_omp_get_num_threads(),
            afs_omp_get_level(),
            afs_omp_get_active_level(),
        ));
    }

    #[test]
    fn nested_serialized_region_restores_parent_team_context() {
        reset_thread_state();
        let observations = NestedObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(outer_task_with_nested_inactive_region),
                &observations as *const NestedObservations as *mut c_void,
                1,
                2,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            observations.states.into_inner().unwrap(),
            vec![(0, 0, 2, 1, 1), (1, 0, 1, 2, 1), (2, 0, 2, 1, 1)]
        );
        assert_eq!(afs_omp_get_level(), 0);
        assert_eq!(afs_omp_get_active_level(), 0);
    }
}
