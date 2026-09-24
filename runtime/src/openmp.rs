//! ARMFORTAS-owned OpenMP host runtime ABI.
//!
//! ABI version 1 uses a synchronous, fixed-signature parallel-region call and
//! compiler-private barrier/static-worksharing entry points:
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
use std::sync::{Arc, Barrier, OnceLock};
use std::time::Instant;

pub const AFS_OMP_ABI_VERSION: u32 = 1;
pub const AFS_OMP_SUCCESS: i32 = 0;
pub const AFS_OMP_ERROR_NULL_ENTRY: i32 = 1;
pub const AFS_OMP_ERROR_UNSUPPORTED_FLAGS: i32 = 2;
pub const AFS_OMP_ERROR_TEAM_PANIC: i32 = 3;
pub const AFS_OMP_ERROR_NO_TEAM: i32 = 4;
pub const AFS_OMP_ERROR_INVALID_LOOP: i32 = 5;

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
    barrier: Arc<Barrier>,
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
    barrier: Arc<Barrier>,
) {
    let _guard = enter_team(
        TeamContext {
            thread_num,
            team_size,
            active: team_size > 1,
            contention_group,
            barrier,
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
    let barrier = Arc::new(Barrier::new(team_size as usize));

    let result = catch_unwind(AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            for thread_num in 1..team_size {
                let contention_group = Arc::clone(&contention_group);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    run_implicit_task(
                        entry,
                        environment,
                        thread_num,
                        team_size,
                        inherited_requested_threads,
                        contention_group,
                        barrier,
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
                Arc::clone(&barrier),
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

/// Wait until every implicit task in the current team reaches this point.
///
/// The barrier is reusable, so successive worksharing constructs in one
/// parallel region share the same team synchronization object.
#[no_mangle]
pub extern "C" fn afs_omp_barrier() -> i32 {
    let barrier = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.barrier))
    });
    let Some(barrier) = barrier else {
        return AFS_OMP_ERROR_NO_TEAM;
    };
    barrier.wait();
    AFS_OMP_SUCCESS
}

/// Compute one thread's contiguous `schedule(static)` iteration interval.
///
/// A positive result means `first` and `last` were written. Zero means this
/// thread has no iterations. A negative result reports an invalid loop/team
/// description. Intermediate arithmetic uses i128 so every representable i64
/// Fortran loop bound is partitioned without overflowing in the runtime.
#[no_mangle]
pub extern "C" fn afs_omp_static_bounds(
    lower: i64,
    upper: i64,
    step: i64,
    thread_num: i32,
    team_size: i32,
    first: *mut i64,
    last: *mut i64,
) -> i32 {
    if step == 0
        || team_size <= 0
        || thread_num < 0
        || thread_num >= team_size
        || first.is_null()
        || last.is_null()
    {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let lower = i128::from(lower);
    let upper = i128::from(upper);
    let step = i128::from(step);
    let iterations = loop_iteration_count(lower, upper, step);
    if iterations == 0 {
        return 0;
    }

    let team_size = i128::from(team_size);
    let thread_num = i128::from(thread_num);
    let base = iterations / team_size;
    let remainder = iterations % team_size;
    let local_iterations = base + i128::from(thread_num < remainder);
    if local_iterations == 0 {
        return 0;
    }
    let first_index = thread_num * base + thread_num.min(remainder);
    let local_first = lower + first_index * step;
    let local_last = local_first + (local_iterations - 1) * step;
    debug_assert!(i64::try_from(local_first).is_ok());
    debug_assert!(i64::try_from(local_last).is_ok());
    unsafe {
        *first = local_first as i64;
        *last = local_last as i64;
    }
    1
}

/// Compute one thread's `chunk_index`th interval for
/// `schedule(static, chunk_size)`.
///
/// Chunks are assigned round-robin in thread-number order. A positive result
/// writes the next interval for this thread, zero means that this thread has
/// no chunk at the requested index, and a negative result reports invalid
/// inputs. As with [`afs_omp_static_bounds`], i128 intermediates keep bound
/// arithmetic defined throughout the representable i64 iteration space.
#[no_mangle]
pub extern "C" fn afs_omp_static_chunk_bounds(
    lower: i64,
    upper: i64,
    step: i64,
    chunk_size: i64,
    thread_num: i32,
    team_size: i32,
    chunk_index: i64,
    first: *mut i64,
    last: *mut i64,
) -> i32 {
    if step == 0
        || chunk_size <= 0
        || team_size <= 0
        || thread_num < 0
        || thread_num >= team_size
        || chunk_index < 0
        || first.is_null()
        || last.is_null()
    {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let lower = i128::from(lower);
    let upper = i128::from(upper);
    let step = i128::from(step);
    let iterations = loop_iteration_count(lower, upper, step);
    let chunk_size = i128::from(chunk_size);
    let global_chunk = i128::from(thread_num) + i128::from(chunk_index) * i128::from(team_size);
    let total_chunks = (iterations + chunk_size - 1) / chunk_size;
    if global_chunk >= total_chunks {
        return 0;
    }
    let first_index = global_chunk * chunk_size;
    let local_iterations = chunk_size.min(iterations - first_index);
    let local_first = lower + first_index * step;
    let local_last = local_first + (local_iterations - 1) * step;
    debug_assert!(i64::try_from(local_first).is_ok());
    debug_assert!(i64::try_from(local_last).is_ok());
    unsafe {
        *first = local_first as i64;
        *last = local_last as i64;
    }
    1
}

/// Compute the rectangular logical shape used by `collapse(2)` lowering.
///
/// The compiler reconstructs the two source indices from a flattened signed
/// i64 logical index. Reject shapes whose individual or product trip counts
/// cannot be represented by that ABI instead of allowing wrapped iteration
/// counts to silently lose work.
#[no_mangle]
pub extern "C" fn afs_omp_collapse2_shape(
    outer_lower: i64,
    outer_upper: i64,
    outer_step: i64,
    inner_lower: i64,
    inner_upper: i64,
    inner_step: i64,
    outer_count: *mut i64,
    inner_count: *mut i64,
    total_count: *mut i64,
) -> i32 {
    if outer_count.is_null() || inner_count.is_null() || total_count.is_null() {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }
    unsafe {
        *outer_count = 0;
        *inner_count = 0;
        *total_count = 0;
    }
    if outer_step == 0 || inner_step == 0 {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let outer_iterations = loop_iteration_count(
        i128::from(outer_lower),
        i128::from(outer_upper),
        i128::from(outer_step),
    );
    let inner_iterations = loop_iteration_count(
        i128::from(inner_lower),
        i128::from(inner_upper),
        i128::from(inner_step),
    );
    let Some(total_iterations) = outer_iterations.checked_mul(inner_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    let Ok(outer_iterations) = i64::try_from(outer_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    let Ok(inner_iterations) = i64::try_from(inner_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    let Ok(total_iterations) = i64::try_from(total_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };

    unsafe {
        *outer_count = outer_iterations;
        *inner_count = inner_iterations;
        *total_count = total_iterations;
    }
    AFS_OMP_SUCCESS
}

/// Reconstruct both source indices for one flattened `collapse(2)` iteration.
///
/// This remains a runtime operation so sparse ranges spanning the signed i64
/// domain use i128 intermediate products rather than overflowing generated
/// target arithmetic.
#[no_mangle]
pub extern "C" fn afs_omp_collapse2_indices(
    flat_index: i64,
    outer_count: i64,
    inner_count: i64,
    outer_lower: i64,
    outer_step: i64,
    inner_lower: i64,
    inner_step: i64,
    outer_value: *mut i64,
    inner_value: *mut i64,
) -> i32 {
    if outer_value.is_null() || inner_value.is_null() {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }
    unsafe {
        *outer_value = 0;
        *inner_value = 0;
    }
    if flat_index < 0 || outer_count <= 0 || inner_count <= 0 || outer_step == 0 || inner_step == 0
    {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let flat_index = i128::from(flat_index);
    let outer_count = i128::from(outer_count);
    let inner_count = i128::from(inner_count);
    if flat_index >= outer_count * inner_count {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }
    let outer_index = flat_index / inner_count;
    let inner_index = flat_index % inner_count;
    let reconstructed_outer = i128::from(outer_lower) + outer_index * i128::from(outer_step);
    let reconstructed_inner = i128::from(inner_lower) + inner_index * i128::from(inner_step);
    let Ok(reconstructed_outer) = i64::try_from(reconstructed_outer) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    let Ok(reconstructed_inner) = i64::try_from(reconstructed_inner) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    unsafe {
        *outer_value = reconstructed_outer;
        *inner_value = reconstructed_inner;
    }
    AFS_OMP_SUCCESS
}

fn loop_iteration_count(lower: i128, upper: i128, step: i128) -> i128 {
    if step > 0 {
        if lower > upper {
            0
        } else {
            ((upper - lower) / step) + 1
        }
    } else if lower < upper {
        0
    } else {
        ((lower - upper) / -step) + 1
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};
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

    struct BarrierObservations {
        arrived: AtomicUsize,
        all_arrived_after_barrier: AtomicBool,
    }

    unsafe extern "C" fn synchronize_task(
        environment: *mut c_void,
        _thread_num: i32,
        team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const BarrierObservations) };
        observations.arrived.fetch_add(1, Ordering::SeqCst);
        assert_eq!(afs_omp_barrier(), AFS_OMP_SUCCESS);
        if observations.arrived.load(Ordering::SeqCst) != team_size as usize {
            observations
                .all_arrived_after_barrier
                .store(false, Ordering::SeqCst);
        }
    }

    #[test]
    fn team_barrier_waits_for_every_implicit_task() {
        reset_thread_state();
        assert_eq!(afs_omp_barrier(), AFS_OMP_ERROR_NO_TEAM);
        let observations = BarrierObservations {
            arrived: AtomicUsize::new(0),
            all_arrived_after_barrier: AtomicBool::new(true),
        };
        assert_eq!(
            afs_omp_parallel_region(
                Some(synchronize_task),
                &observations as *const BarrierObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(observations.arrived.load(Ordering::SeqCst), 4);
        assert!(observations
            .all_arrived_after_barrier
            .load(Ordering::SeqCst));
    }

    fn static_bounds(
        lower: i64,
        upper: i64,
        step: i64,
        thread_num: i32,
        team_size: i32,
    ) -> Option<(i64, i64)> {
        let mut first = 0;
        let mut last = 0;
        let status = afs_omp_static_bounds(
            lower, upper, step, thread_num, team_size, &mut first, &mut last,
        );
        assert!(status >= 0, "unexpected static-bounds error {status}");
        (status == 1).then_some((first, last))
    }

    fn static_chunk_bounds(
        lower: i64,
        upper: i64,
        step: i64,
        chunk_size: i64,
        thread_num: i32,
        team_size: i32,
        chunk_index: i64,
    ) -> Option<(i64, i64)> {
        let mut first = 0;
        let mut last = 0;
        let status = afs_omp_static_chunk_bounds(
            lower,
            upper,
            step,
            chunk_size,
            thread_num,
            team_size,
            chunk_index,
            &mut first,
            &mut last,
        );
        assert!(status >= 0, "unexpected static-chunk error {status}");
        (status == 1).then_some((first, last))
    }

    #[test]
    fn static_bounds_partition_positive_and_negative_iteration_spaces() {
        assert_eq!(static_bounds(1, 10, 1, 0, 3), Some((1, 4)));
        assert_eq!(static_bounds(1, 10, 1, 1, 3), Some((5, 7)));
        assert_eq!(static_bounds(1, 10, 1, 2, 3), Some((8, 10)));

        assert_eq!(static_bounds(10, -2, -3, 0, 3), Some((10, 7)));
        assert_eq!(static_bounds(10, -2, -3, 1, 3), Some((4, 1)));
        assert_eq!(static_bounds(10, -2, -3, 2, 3), Some((-2, -2)));
        assert_eq!(static_bounds(1, 0, 1, 0, 4), None);
        assert_eq!(static_bounds(0, 1, -1, 0, 4), None);
    }

    #[test]
    fn static_bounds_handle_sparse_and_extreme_i64_ranges() {
        assert_eq!(static_bounds(2, 2, 1, 0, 4), Some((2, 2)));
        assert_eq!(static_bounds(2, 2, 1, 1, 4), None);
        assert_eq!(
            static_bounds(i64::MIN, i64::MAX, i64::MAX, 0, 2),
            Some((i64::MIN, -1))
        );
        assert_eq!(
            static_bounds(i64::MIN, i64::MAX, i64::MAX, 1, 2),
            Some((i64::MAX - 1, i64::MAX - 1))
        );
        assert_eq!(
            afs_omp_static_bounds(1, 2, 0, 0, 1, std::ptr::null_mut(), std::ptr::null_mut()),
            -AFS_OMP_ERROR_INVALID_LOOP
        );
    }

    #[test]
    fn static_chunk_bounds_assign_round_robin_positive_and_negative_chunks() {
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 0, 3, 0), Some((1, 2)));
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 1, 3, 0), Some((3, 4)));
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 2, 3, 0), Some((5, 6)));
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 0, 3, 1), Some((7, 8)));
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 1, 3, 1), Some((9, 10)));
        assert_eq!(static_chunk_bounds(1, 10, 1, 2, 2, 3, 1), None);

        assert_eq!(static_chunk_bounds(10, -2, -3, 2, 0, 3, 0), Some((10, 7)));
        assert_eq!(static_chunk_bounds(10, -2, -3, 2, 1, 3, 0), Some((4, 1)));
        assert_eq!(static_chunk_bounds(10, -2, -3, 2, 2, 3, 0), Some((-2, -2)));
    }

    #[test]
    fn static_chunk_bounds_reject_invalid_descriptions_and_handle_i64_bounds() {
        assert_eq!(
            static_chunk_bounds(i64::MIN, i64::MAX, i64::MAX, 2, 0, 2, 0),
            Some((i64::MIN, -1))
        );
        assert_eq!(
            static_chunk_bounds(i64::MIN, i64::MAX, i64::MAX, 2, 1, 2, 0),
            Some((i64::MAX - 1, i64::MAX - 1))
        );
        assert_eq!(
            static_chunk_bounds(1, 10, 1, i64::MAX, i32::MAX - 1, i32::MAX, i64::MAX),
            None
        );
        let mut first = 0;
        let mut last = 0;
        for invalid_chunk in [0, -1] {
            assert_eq!(
                afs_omp_static_chunk_bounds(
                    1,
                    10,
                    1,
                    invalid_chunk,
                    0,
                    2,
                    0,
                    &mut first,
                    &mut last,
                ),
                -AFS_OMP_ERROR_INVALID_LOOP
            );
        }
    }

    fn collapse2_shape(
        outer: (i64, i64, i64),
        inner: (i64, i64, i64),
    ) -> Result<(i64, i64, i64), i32> {
        let mut outer_count = -1;
        let mut inner_count = -1;
        let mut total_count = -1;
        let status = afs_omp_collapse2_shape(
            outer.0,
            outer.1,
            outer.2,
            inner.0,
            inner.1,
            inner.2,
            &mut outer_count,
            &mut inner_count,
            &mut total_count,
        );
        if status == AFS_OMP_SUCCESS {
            Ok((outer_count, inner_count, total_count))
        } else {
            Err(status)
        }
    }

    #[test]
    fn collapse2_shape_flattens_rectangular_positive_and_negative_spaces() {
        assert_eq!(collapse2_shape((1, 3, 1), (8, 2, -2)), Ok((3, 4, 12)));
        assert_eq!(collapse2_shape((3, 1, -1), (-2, 2, 2)), Ok((3, 3, 9)));
        assert_eq!(collapse2_shape((1, 0, 1), (1, 4, 1)), Ok((0, 4, 0)));
        assert_eq!(collapse2_shape((1, 4, 1), (0, 1, -1)), Ok((4, 0, 0)));
    }

    #[test]
    fn collapse2_shape_rejects_zero_steps_and_unrepresentable_products() {
        assert_eq!(
            collapse2_shape((1, 2, 0), (1, 2, 1)),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            collapse2_shape((i64::MIN, i64::MAX, 1), (1, 0, 1)),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            collapse2_shape((0, i64::MAX, 1), (1, 2, 1)),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        let mut outer_count = -1;
        let mut inner_count = -1;
        let mut total_count = -1;
        assert_eq!(
            afs_omp_collapse2_shape(
                1,
                2,
                0,
                1,
                2,
                1,
                &mut outer_count,
                &mut inner_count,
                &mut total_count,
            ),
            -AFS_OMP_ERROR_INVALID_LOOP
        );
        assert_eq!((outer_count, inner_count, total_count), (0, 0, 0));
    }

    fn collapse2_indices(
        flat_index: i64,
        shape: (i64, i64),
        outer: (i64, i64),
        inner: (i64, i64),
    ) -> Result<(i64, i64), i32> {
        let mut outer_value = -1;
        let mut inner_value = -1;
        let status = afs_omp_collapse2_indices(
            flat_index,
            shape.0,
            shape.1,
            outer.0,
            outer.1,
            inner.0,
            inner.1,
            &mut outer_value,
            &mut inner_value,
        );
        if status == AFS_OMP_SUCCESS {
            Ok((outer_value, inner_value))
        } else {
            Err(status)
        }
    }

    #[test]
    fn collapse2_indices_reconstruct_order_without_i64_intermediate_overflow() {
        assert_eq!(collapse2_indices(0, (3, 2), (1, 2), (8, -3)), Ok((1, 8)));
        assert_eq!(collapse2_indices(3, (3, 2), (1, 2), (8, -3)), Ok((3, 5)));
        assert_eq!(collapse2_indices(5, (3, 2), (1, 2), (8, -3)), Ok((5, 5)));
        assert_eq!(
            collapse2_indices(4, (3, 2), (i64::MIN, i64::MAX), (i64::MAX, -i64::MAX),),
            Ok((i64::MAX - 1, i64::MAX))
        );
        assert_eq!(
            collapse2_indices(6, (3, 2), (1, 1), (1, 1)),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
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

    type NestedState = (i32, i32, i32, i32, i32);

    #[derive(Default)]
    struct NestedObservations {
        states: Mutex<Vec<NestedState>>,
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
