//! ARMFORTAS-owned OpenMP host runtime ABI.
//!
//! ABI version 1 uses a synchronous, fixed-signature parallel-region call and
//! compiler-private barrier/worksharing entry points:
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
use std::sync::{Arc, Barrier, Condvar, Mutex, OnceLock};
use std::time::Instant;

pub const AFS_OMP_ABI_VERSION: u32 = 1;
pub const AFS_OMP_SUCCESS: i32 = 0;
pub const AFS_OMP_ERROR_NULL_ENTRY: i32 = 1;
pub const AFS_OMP_ERROR_UNSUPPORTED_FLAGS: i32 = 2;
pub const AFS_OMP_ERROR_TEAM_PANIC: i32 = 3;
pub const AFS_OMP_ERROR_NO_TEAM: i32 = 4;
pub const AFS_OMP_ERROR_INVALID_LOOP: i32 = 5;
pub const AFS_OMP_ERROR_INVALID_REDUCTION: i32 = 6;
pub const AFS_OMP_ERROR_INVALID_CRITICAL: i32 = 7;

pub const AFS_OMP_REDUCTION_ADD: i32 = 1;
pub const AFS_OMP_REDUCTION_MULTIPLY: i32 = 2;
pub const AFS_OMP_REDUCTION_MAX: i32 = 3;
pub const AFS_OMP_REDUCTION_MIN: i32 = 4;
pub const AFS_OMP_REDUCTION_AND: i32 = 5;
pub const AFS_OMP_REDUCTION_OR: i32 = 6;
pub const AFS_OMP_REDUCTION_EQV: i32 = 7;
pub const AFS_OMP_REDUCTION_NEQV: i32 = 8;
pub const AFS_OMP_REDUCTION_IAND: i32 = 9;
pub const AFS_OMP_REDUCTION_IOR: i32 = 10;
pub const AFS_OMP_REDUCTION_IEOR: i32 = 11;

pub const AFS_OMP_REDUCTION_KIND_I8: i32 = 1;
pub const AFS_OMP_REDUCTION_KIND_I16: i32 = 2;
pub const AFS_OMP_REDUCTION_KIND_I32: i32 = 3;
pub const AFS_OMP_REDUCTION_KIND_I64: i32 = 4;
pub const AFS_OMP_REDUCTION_KIND_F32: i32 = 5;
pub const AFS_OMP_REDUCTION_KIND_F64: i32 = 6;
pub const AFS_OMP_REDUCTION_KIND_C32: i32 = 7;
pub const AFS_OMP_REDUCTION_KIND_C64: i32 = 8;

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
    reduction: Arc<Mutex<ReductionWorkspace>>,
    dynamic: Arc<Mutex<DynamicWorkspace>>,
}

#[derive(Debug, Default)]
struct ReductionWorkspace {
    slots: Vec<i64>,
    result: i64,
    real_slots: Vec<u64>,
    real_result: u64,
    complex_slots: Vec<[u64; 2]>,
    complex_result: [u64; 2],
    array_slots: Vec<Vec<u8>>,
    array_result: Vec<u8>,
    array_status: i32,
}

#[derive(Debug, Default)]
struct DynamicWorkspace {
    next_index: i128,
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
    reduction: Arc<Mutex<ReductionWorkspace>>,
    dynamic: Arc<Mutex<DynamicWorkspace>>,
) {
    let _guard = enter_team(
        TeamContext {
            thread_num,
            team_size,
            active: team_size > 1,
            contention_group,
            barrier,
            reduction,
            dynamic,
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
    let reduction = Arc::new(Mutex::new(ReductionWorkspace::default()));
    let dynamic = Arc::new(Mutex::new(DynamicWorkspace::default()));

    let result = catch_unwind(AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            for thread_num in 1..team_size {
                let contention_group = Arc::clone(&contention_group);
                let barrier = Arc::clone(&barrier);
                let reduction = Arc::clone(&reduction);
                let dynamic = Arc::clone(&dynamic);
                scope.spawn(move || {
                    run_implicit_task(
                        entry,
                        environment,
                        thread_num,
                        team_size,
                        inherited_requested_threads,
                        contention_group,
                        barrier,
                        reduction,
                        dynamic,
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
                Arc::clone(&reduction),
                Arc::clone(&dynamic),
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

#[derive(Debug, Default)]
struct CriticalLock {
    held: Mutex<bool>,
    available: Condvar,
}

fn critical_lock(name: *const u8, name_len: i64) -> Result<Arc<CriticalLock>, i32> {
    let Ok(name_len) = usize::try_from(name_len) else {
        return Err(AFS_OMP_ERROR_INVALID_CRITICAL);
    };
    if name_len > 0 && name.is_null() {
        return Err(AFS_OMP_ERROR_INVALID_CRITICAL);
    }
    let mut key = if name_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(name, name_len) }.to_vec()
    };
    key.make_ascii_lowercase();

    static LOCKS: OnceLock<Mutex<std::collections::HashMap<Vec<u8>, Arc<CriticalLock>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(Arc::clone(locks.entry(key).or_default()))
}

/// Enter a named or unnamed OpenMP critical region.
///
/// Lock identity is process-wide and based on the case-insensitive Fortran
/// name bytes. A zero-length name denotes the single global unnamed critical
/// region. The registry lives in the runtime rather than generated objects so
/// separately compiled procedures resolve the same name to the same lock.
#[no_mangle]
pub extern "C" fn afs_omp_critical_enter(name: *const u8, name_len: i64) -> i32 {
    let Ok(lock) = critical_lock(name, name_len) else {
        return AFS_OMP_ERROR_INVALID_CRITICAL;
    };
    let mut held = lock
        .held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *held {
        held = lock
            .available
            .wait(held)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *held = true;
    AFS_OMP_SUCCESS
}

/// Leave the matching OpenMP critical region.
#[no_mangle]
pub extern "C" fn afs_omp_critical_exit(name: *const u8, name_len: i64) -> i32 {
    let Ok(lock) = critical_lock(name, name_len) else {
        return AFS_OMP_ERROR_INVALID_CRITICAL;
    };
    let mut held = lock
        .held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !*held {
        return AFS_OMP_ERROR_INVALID_CRITICAL;
    }
    *held = false;
    drop(held);
    lock.available.notify_one();
    AFS_OMP_SUCCESS
}

fn combine_i64_reduction(operator: i32, left: i64, right: i64) -> Option<i64> {
    match operator {
        AFS_OMP_REDUCTION_ADD => Some(left.wrapping_add(right)),
        AFS_OMP_REDUCTION_MULTIPLY => Some(left.wrapping_mul(right)),
        AFS_OMP_REDUCTION_MAX => Some(left.max(right)),
        AFS_OMP_REDUCTION_MIN => Some(left.min(right)),
        AFS_OMP_REDUCTION_AND => Some(i64::from(left != 0 && right != 0)),
        AFS_OMP_REDUCTION_OR => Some(i64::from(left != 0 || right != 0)),
        AFS_OMP_REDUCTION_EQV => Some(i64::from((left != 0) == (right != 0))),
        AFS_OMP_REDUCTION_NEQV => Some(i64::from((left != 0) != (right != 0))),
        AFS_OMP_REDUCTION_IAND => Some(left & right),
        AFS_OMP_REDUCTION_IOR => Some(left | right),
        AFS_OMP_REDUCTION_IEOR => Some(left ^ right),
        _ => None,
    }
}

trait RealReductionValue: Copy {
    fn zero() -> Self;
    fn encode(self) -> u64;
    fn decode(bits: u64) -> Self;
    fn combine(operator: i32, left: Self, right: Self) -> Option<Self>;
}

impl RealReductionValue for f32 {
    fn zero() -> Self {
        0.0
    }

    fn encode(self) -> u64 {
        u64::from(self.to_bits())
    }

    fn decode(bits: u64) -> Self {
        Self::from_bits(bits as u32)
    }

    fn combine(operator: i32, left: Self, right: Self) -> Option<Self> {
        match operator {
            AFS_OMP_REDUCTION_ADD => Some(left + right),
            AFS_OMP_REDUCTION_MULTIPLY => Some(left * right),
            AFS_OMP_REDUCTION_MAX => Some(left.max(right)),
            AFS_OMP_REDUCTION_MIN => Some(left.min(right)),
            _ => None,
        }
    }
}

impl RealReductionValue for f64 {
    fn zero() -> Self {
        0.0
    }

    fn encode(self) -> u64 {
        self.to_bits()
    }

    fn decode(bits: u64) -> Self {
        Self::from_bits(bits)
    }

    fn combine(operator: i32, left: Self, right: Self) -> Option<Self> {
        match operator {
            AFS_OMP_REDUCTION_ADD => Some(left + right),
            AFS_OMP_REDUCTION_MULTIPLY => Some(left * right),
            AFS_OMP_REDUCTION_MAX => Some(left.max(right)),
            AFS_OMP_REDUCTION_MIN => Some(left.min(right)),
            _ => None,
        }
    }
}

trait ComplexReductionValue: Copy {
    fn zero() -> Self;
    fn encode(self) -> [u64; 2];
    fn decode(bits: [u64; 2]) -> Self;
    fn combine(operator: i32, left: Self, right: Self) -> Option<Self>;
}

impl ComplexReductionValue for [f32; 2] {
    fn zero() -> Self {
        [0.0, 0.0]
    }

    fn encode(self) -> [u64; 2] {
        [u64::from(self[0].to_bits()), u64::from(self[1].to_bits())]
    }

    fn decode(bits: [u64; 2]) -> Self {
        [
            f32::from_bits(bits[0] as u32),
            f32::from_bits(bits[1] as u32),
        ]
    }

    fn combine(operator: i32, left: Self, right: Self) -> Option<Self> {
        match operator {
            AFS_OMP_REDUCTION_ADD => Some([left[0] + right[0], left[1] + right[1]]),
            AFS_OMP_REDUCTION_MULTIPLY => Some([
                left[0] * right[0] - left[1] * right[1],
                left[0] * right[1] + left[1] * right[0],
            ]),
            _ => None,
        }
    }
}

impl ComplexReductionValue for [f64; 2] {
    fn zero() -> Self {
        [0.0, 0.0]
    }

    fn encode(self) -> [u64; 2] {
        [self[0].to_bits(), self[1].to_bits()]
    }

    fn decode(bits: [u64; 2]) -> Self {
        [f64::from_bits(bits[0]), f64::from_bits(bits[1])]
    }

    fn combine(operator: i32, left: Self, right: Self) -> Option<Self> {
        match operator {
            AFS_OMP_REDUCTION_ADD => Some([left[0] + right[0], left[1] + right[1]]),
            AFS_OMP_REDUCTION_MULTIPLY => Some([
                left[0] * right[0] - left[1] * right[1],
                left[0] * right[1] + left[1] * right[0],
            ]),
            _ => None,
        }
    }
}

fn reduce_real<T: RealReductionValue>(
    operator: i32,
    private_value: T,
    original_value: T,
) -> Result<T, i32> {
    if T::combine(operator, T::zero(), T::zero()).is_none() {
        return Err(AFS_OMP_ERROR_INVALID_REDUCTION);
    }
    let context = THREAD_STATE.with(|state| {
        let state = state.borrow();
        state.teams.last().map(|team| {
            (
                team.thread_num,
                team.team_size,
                Arc::clone(&team.barrier),
                Arc::clone(&team.reduction),
            )
        })
    });
    let Some((thread_num, team_size, barrier, reduction)) = context else {
        return Err(AFS_OMP_ERROR_NO_TEAM);
    };

    {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let team_size = usize::try_from(team_size).unwrap_or(0);
        if workspace.real_slots.len() != team_size {
            workspace.real_slots.resize(team_size, T::zero().encode());
        }
        workspace.real_slots[thread_num as usize] = private_value.encode();
    }
    barrier.wait();

    if thread_num == 0 {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let combined =
            workspace
                .real_slots
                .iter()
                .copied()
                .fold(original_value, |combined, value| {
                    T::combine(operator, combined, T::decode(value))
                        .expect("validated OpenMP REAL reduction operator became invalid")
                });
        workspace.real_result = combined.encode();
    }
    barrier.wait();

    let combined = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .real_result;
    Ok(T::decode(combined))
}

/// Combine one REAL(4) private value across the current team.
///
/// The runtime performs every combiner operation at binary32 precision and in
/// ascending thread-number order. The latter is deterministic but not exposed
/// as a language guarantee: OpenMP leaves the reduction order unspecified.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_f32(
    operator: i32,
    private_value: f32,
    original_value: f32,
    result: *mut f32,
) -> i32 {
    if result.is_null() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let combined = match reduce_real(operator, private_value, original_value) {
        Ok(combined) => combined,
        Err(error) => return error,
    };
    unsafe {
        result.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

/// Combine one REAL(8) or DOUBLE PRECISION private value across the current
/// team using binary64 operations.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_f64(
    operator: i32,
    private_value: f64,
    original_value: f64,
    result: *mut f64,
) -> i32 {
    if result.is_null() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let combined = match reduce_real(operator, private_value, original_value) {
        Ok(combined) => combined,
        Err(error) => return error,
    };
    unsafe {
        result.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

fn reduce_complex<T: ComplexReductionValue>(
    operator: i32,
    private_value: T,
    original_value: T,
) -> Result<T, i32> {
    if T::combine(operator, T::zero(), T::zero()).is_none() {
        return Err(AFS_OMP_ERROR_INVALID_REDUCTION);
    }
    let context = THREAD_STATE.with(|state| {
        let state = state.borrow();
        state.teams.last().map(|team| {
            (
                team.thread_num,
                team.team_size,
                Arc::clone(&team.barrier),
                Arc::clone(&team.reduction),
            )
        })
    });
    let Some((thread_num, team_size, barrier, reduction)) = context else {
        return Err(AFS_OMP_ERROR_NO_TEAM);
    };

    {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let team_size = usize::try_from(team_size).unwrap_or(0);
        if workspace.complex_slots.len() != team_size {
            workspace
                .complex_slots
                .resize(team_size, T::zero().encode());
        }
        workspace.complex_slots[thread_num as usize] = private_value.encode();
    }
    barrier.wait();

    if thread_num == 0 {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let combined =
            workspace
                .complex_slots
                .iter()
                .copied()
                .fold(original_value, |combined, value| {
                    T::combine(operator, combined, T::decode(value))
                        .expect("validated OpenMP COMPLEX reduction operator became invalid")
                });
        workspace.complex_result = combined.encode();
    }
    barrier.wait();

    let combined = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .complex_result;
    Ok(T::decode(combined))
}

/// Combine one COMPLEX(4) private value across the current team.
///
/// Values cross the runtime boundary as two-lane buffers so this ABI does not
/// depend on a platform's aggregate argument or return convention.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_c32(
    operator: i32,
    private_value: *const [f32; 2],
    original_value: *const [f32; 2],
    result: *mut [f32; 2],
) -> i32 {
    if private_value.is_null() || original_value.is_null() || result.is_null() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let private_value = unsafe { private_value.read_unaligned() };
    let original_value = unsafe { original_value.read_unaligned() };
    let combined = match reduce_complex(operator, private_value, original_value) {
        Ok(combined) => combined,
        Err(error) => return error,
    };
    unsafe {
        result.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

/// Combine one COMPLEX(8) or DOUBLE COMPLEX private value across the current
/// team using binary64 component operations.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_c64(
    operator: i32,
    private_value: *const [f64; 2],
    original_value: *const [f64; 2],
    result: *mut [f64; 2],
) -> i32 {
    if private_value.is_null() || original_value.is_null() || result.is_null() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let private_value = unsafe { private_value.read_unaligned() };
    let original_value = unsafe { original_value.read_unaligned() };
    let combined = match reduce_complex(operator, private_value, original_value) {
        Ok(combined) => combined,
        Err(error) => return error,
    };
    unsafe {
        result.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

/// Combine one signed-integer or logical private value across the current team.
///
/// Every implicit task must call this entry point in the same order. Values are
/// stored by thread number and thread zero combines the original value followed
/// by private values in ascending thread-number order. The two team barriers
/// make the result available to every caller before the workspace is reused by
/// a later reduction.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_i64(
    operator: i32,
    private_value: i64,
    original_value: i64,
    result: *mut i64,
) -> i32 {
    if result.is_null() || combine_i64_reduction(operator, 0, 0).is_none() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let context = THREAD_STATE.with(|state| {
        let state = state.borrow();
        state.teams.last().map(|team| {
            (
                team.thread_num,
                team.team_size,
                Arc::clone(&team.barrier),
                Arc::clone(&team.reduction),
            )
        })
    });
    let Some((thread_num, team_size, barrier, reduction)) = context else {
        return AFS_OMP_ERROR_NO_TEAM;
    };

    {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let team_size = usize::try_from(team_size).unwrap_or(0);
        if workspace.slots.len() != team_size {
            workspace.slots.resize(team_size, 0);
        }
        workspace.slots[thread_num as usize] = private_value;
    }
    barrier.wait();

    if thread_num == 0 {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        workspace.result =
            workspace
                .slots
                .iter()
                .copied()
                .fold(original_value, |combined, value| {
                    combine_i64_reduction(operator, combined, value)
                        .expect("validated OpenMP reduction operator became invalid")
                });
    }
    barrier.wait();

    let combined = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .result;
    unsafe {
        *result = combined;
    }
    AFS_OMP_SUCCESS
}

fn valid_i64_reduction_storage_bytes(storage_bytes: i32) -> bool {
    matches!(storage_bytes, 1 | 2 | 4 | 8)
}

unsafe fn read_i64_reduction_storage(storage: *const c_void, storage_bytes: i32) -> i64 {
    match storage_bytes {
        1 => i64::from(unsafe { (storage.cast::<i8>()).read_unaligned() }),
        2 => i64::from(unsafe { (storage.cast::<i16>()).read_unaligned() }),
        4 => i64::from(unsafe { (storage.cast::<i32>()).read_unaligned() }),
        8 => unsafe { (storage.cast::<i64>()).read_unaligned() },
        _ => unreachable!("validated OpenMP reduction storage width became invalid"),
    }
}

unsafe fn write_i64_reduction_storage(storage: *mut c_void, storage_bytes: i32, value: i64) {
    match storage_bytes {
        1 => unsafe { storage.cast::<i8>().write_unaligned(value as i8) },
        2 => unsafe { storage.cast::<i16>().write_unaligned(value as i16) },
        4 => unsafe { storage.cast::<i32>().write_unaligned(value as i32) },
        8 => unsafe { storage.cast::<i64>().write_unaligned(value) },
        _ => unreachable!("validated OpenMP reduction storage width became invalid"),
    }
}

/// Atomically combine one signed-integer or logical private value into the
/// original object without a team barrier.
///
/// This entry point implements the end of a worksharing reduction carrying a
/// `NOWAIT` clause. The team reduction mutex serializes the read/modify/write,
/// but each implicit task returns as soon as its own private value has been
/// combined. A later program synchronization point is therefore still needed
/// before another task may safely consume the final value.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_i64_nowait(
    operator: i32,
    private_value: i64,
    original_storage: *mut c_void,
    storage_bytes: i32,
) -> i32 {
    if original_storage.is_null()
        || !valid_i64_reduction_storage_bytes(storage_bytes)
        || combine_i64_reduction(operator, 0, 0).is_none()
    {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let reduction = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.reduction))
    });
    let Some(reduction) = reduction else {
        return AFS_OMP_ERROR_NO_TEAM;
    };

    let _workspace = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original = unsafe { read_i64_reduction_storage(original_storage, storage_bytes) };
    let combined = combine_i64_reduction(operator, original, private_value)
        .expect("validated OpenMP reduction operator became invalid");
    unsafe {
        write_i64_reduction_storage(original_storage, storage_bytes, combined);
    }
    AFS_OMP_SUCCESS
}

fn reduce_real_nowait<T: RealReductionValue>(
    operator: i32,
    private_value: T,
    original_storage: *mut T,
) -> i32 {
    if original_storage.is_null() || T::combine(operator, T::zero(), T::zero()).is_none() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let reduction = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.reduction))
    });
    let Some(reduction) = reduction else {
        return AFS_OMP_ERROR_NO_TEAM;
    };

    let _workspace = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original = unsafe { original_storage.read_unaligned() };
    let combined = T::combine(operator, original, private_value)
        .expect("validated OpenMP REAL reduction operator became invalid");
    unsafe {
        original_storage.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

/// Combine one REAL(4) private value directly into the shared object without
/// a team barrier. This is the `NOWAIT` counterpart of
/// [`afs_omp_reduce_f32`].
#[no_mangle]
pub extern "C" fn afs_omp_reduce_f32_nowait(
    operator: i32,
    private_value: f32,
    original_storage: *mut f32,
) -> i32 {
    reduce_real_nowait(operator, private_value, original_storage)
}

/// Combine one REAL(8) or DOUBLE PRECISION private value directly into the
/// shared object without a team barrier.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_f64_nowait(
    operator: i32,
    private_value: f64,
    original_storage: *mut f64,
) -> i32 {
    reduce_real_nowait(operator, private_value, original_storage)
}

fn reduce_complex_nowait<T: ComplexReductionValue>(
    operator: i32,
    private_value: *const T,
    original_storage: *mut T,
) -> i32 {
    if private_value.is_null()
        || original_storage.is_null()
        || T::combine(operator, T::zero(), T::zero()).is_none()
    {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let reduction = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.reduction))
    });
    let Some(reduction) = reduction else {
        return AFS_OMP_ERROR_NO_TEAM;
    };

    let _workspace = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let private_value = unsafe { private_value.read_unaligned() };
    let original = unsafe { original_storage.read_unaligned() };
    let combined = T::combine(operator, original, private_value)
        .expect("validated OpenMP COMPLEX reduction operator became invalid");
    unsafe {
        original_storage.write_unaligned(combined);
    }
    AFS_OMP_SUCCESS
}

/// Combine one COMPLEX(4) private value directly into the shared object
/// without a team barrier.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_c32_nowait(
    operator: i32,
    private_value: *const [f32; 2],
    original_storage: *mut [f32; 2],
) -> i32 {
    reduce_complex_nowait(operator, private_value, original_storage)
}

/// Combine one COMPLEX(8) or DOUBLE COMPLEX private value directly into the
/// shared object without a team barrier.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_c64_nowait(
    operator: i32,
    private_value: *const [f64; 2],
    original_storage: *mut [f64; 2],
) -> i32 {
    reduce_complex_nowait(operator, private_value, original_storage)
}

fn reduction_array_element_bytes(kind: i32) -> Option<usize> {
    match kind {
        AFS_OMP_REDUCTION_KIND_I8 => Some(1),
        AFS_OMP_REDUCTION_KIND_I16 => Some(2),
        AFS_OMP_REDUCTION_KIND_I32 | AFS_OMP_REDUCTION_KIND_F32 => Some(4),
        AFS_OMP_REDUCTION_KIND_I64 | AFS_OMP_REDUCTION_KIND_F64 => Some(8),
        AFS_OMP_REDUCTION_KIND_C32 => Some(8),
        AFS_OMP_REDUCTION_KIND_C64 => Some(16),
        _ => None,
    }
}

fn valid_reduction_array_operator(operator: i32, kind: i32) -> bool {
    match kind {
        AFS_OMP_REDUCTION_KIND_I8
        | AFS_OMP_REDUCTION_KIND_I16
        | AFS_OMP_REDUCTION_KIND_I32
        | AFS_OMP_REDUCTION_KIND_I64 => matches!(
            operator,
            AFS_OMP_REDUCTION_ADD
                | AFS_OMP_REDUCTION_MULTIPLY
                | AFS_OMP_REDUCTION_MAX
                | AFS_OMP_REDUCTION_MIN
                | AFS_OMP_REDUCTION_AND
                | AFS_OMP_REDUCTION_OR
                | AFS_OMP_REDUCTION_EQV
                | AFS_OMP_REDUCTION_NEQV
                | AFS_OMP_REDUCTION_IAND
                | AFS_OMP_REDUCTION_IOR
                | AFS_OMP_REDUCTION_IEOR
        ),
        AFS_OMP_REDUCTION_KIND_F32 | AFS_OMP_REDUCTION_KIND_F64 => matches!(
            operator,
            AFS_OMP_REDUCTION_ADD
                | AFS_OMP_REDUCTION_MULTIPLY
                | AFS_OMP_REDUCTION_MAX
                | AFS_OMP_REDUCTION_MIN
        ),
        AFS_OMP_REDUCTION_KIND_C32 | AFS_OMP_REDUCTION_KIND_C64 => {
            matches!(operator, AFS_OMP_REDUCTION_ADD | AFS_OMP_REDUCTION_MULTIPLY)
        }
        _ => false,
    }
}

fn reduction_array_byte_len(kind: i32, count: i64) -> Option<usize> {
    let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
    let bytes = count.checked_mul(reduction_array_element_bytes(kind)?)?;
    (bytes <= isize::MAX as usize).then_some(bytes)
}

unsafe fn read_reduction_element<T: Copy>(bytes: &[u8], offset: usize) -> T {
    unsafe { bytes.as_ptr().add(offset).cast::<T>().read_unaligned() }
}

unsafe fn write_reduction_element<T>(bytes: &mut [u8], offset: usize, value: T) {
    unsafe {
        bytes
            .as_mut_ptr()
            .add(offset)
            .cast::<T>()
            .write_unaligned(value)
    }
}

macro_rules! combine_integer_array {
    ($operator:expr, $left:expr, $right:expr, $ty:ty) => {{
        let width = std::mem::size_of::<$ty>();
        for offset in (0..$left.len()).step_by(width) {
            let left_value = unsafe { read_reduction_element::<$ty>($left, offset) };
            let right_value = unsafe { read_reduction_element::<$ty>($right, offset) };
            let combined = match $operator {
                AFS_OMP_REDUCTION_ADD => left_value.wrapping_add(right_value),
                AFS_OMP_REDUCTION_MULTIPLY => left_value.wrapping_mul(right_value),
                AFS_OMP_REDUCTION_MAX => left_value.max(right_value),
                AFS_OMP_REDUCTION_MIN => left_value.min(right_value),
                AFS_OMP_REDUCTION_AND => <$ty>::from(left_value != 0 && right_value != 0),
                AFS_OMP_REDUCTION_OR => <$ty>::from(left_value != 0 || right_value != 0),
                AFS_OMP_REDUCTION_EQV => <$ty>::from((left_value != 0) == (right_value != 0)),
                AFS_OMP_REDUCTION_NEQV => <$ty>::from((left_value != 0) != (right_value != 0)),
                AFS_OMP_REDUCTION_IAND => left_value & right_value,
                AFS_OMP_REDUCTION_IOR => left_value | right_value,
                AFS_OMP_REDUCTION_IEOR => left_value ^ right_value,
                _ => return false,
            };
            unsafe { write_reduction_element($left, offset, combined) };
        }
        true
    }};
}

macro_rules! combine_real_array {
    ($operator:expr, $left:expr, $right:expr, $ty:ty) => {{
        let width = std::mem::size_of::<$ty>();
        for offset in (0..$left.len()).step_by(width) {
            let left_value = unsafe { read_reduction_element::<$ty>($left, offset) };
            let right_value = unsafe { read_reduction_element::<$ty>($right, offset) };
            let combined = match $operator {
                AFS_OMP_REDUCTION_ADD => left_value + right_value,
                AFS_OMP_REDUCTION_MULTIPLY => left_value * right_value,
                AFS_OMP_REDUCTION_MAX => left_value.max(right_value),
                AFS_OMP_REDUCTION_MIN => left_value.min(right_value),
                _ => return false,
            };
            unsafe { write_reduction_element($left, offset, combined) };
        }
        true
    }};
}

macro_rules! combine_complex_array {
    ($operator:expr, $left:expr, $right:expr, $ty:ty) => {{
        let width = std::mem::size_of::<[$ty; 2]>();
        for offset in (0..$left.len()).step_by(width) {
            let left_value = unsafe { read_reduction_element::<[$ty; 2]>($left, offset) };
            let right_value = unsafe { read_reduction_element::<[$ty; 2]>($right, offset) };
            let combined = match $operator {
                AFS_OMP_REDUCTION_ADD => [
                    left_value[0] + right_value[0],
                    left_value[1] + right_value[1],
                ],
                AFS_OMP_REDUCTION_MULTIPLY => [
                    left_value[0] * right_value[0] - left_value[1] * right_value[1],
                    left_value[0] * right_value[1] + left_value[1] * right_value[0],
                ],
                _ => return false,
            };
            unsafe { write_reduction_element($left, offset, combined) };
        }
        true
    }};
}

fn combine_reduction_arrays(operator: i32, kind: i32, left: &mut [u8], right: &[u8]) -> bool {
    if left.len() != right.len() || !valid_reduction_array_operator(operator, kind) {
        return false;
    }
    match kind {
        AFS_OMP_REDUCTION_KIND_I8 => combine_integer_array!(operator, left, right, i8),
        AFS_OMP_REDUCTION_KIND_I16 => combine_integer_array!(operator, left, right, i16),
        AFS_OMP_REDUCTION_KIND_I32 => combine_integer_array!(operator, left, right, i32),
        AFS_OMP_REDUCTION_KIND_I64 => combine_integer_array!(operator, left, right, i64),
        AFS_OMP_REDUCTION_KIND_F32 => combine_real_array!(operator, left, right, f32),
        AFS_OMP_REDUCTION_KIND_F64 => combine_real_array!(operator, left, right, f64),
        AFS_OMP_REDUCTION_KIND_C32 => combine_complex_array!(operator, left, right, f32),
        AFS_OMP_REDUCTION_KIND_C64 => combine_complex_array!(operator, left, right, f64),
        _ => false,
    }
}

fn reduction_array_identity(operator: i32, kind: i32) -> Option<Vec<u8>> {
    macro_rules! scalar_identity {
        ($ty:ty) => {{
            let value: $ty = match operator {
                AFS_OMP_REDUCTION_ADD
                | AFS_OMP_REDUCTION_OR
                | AFS_OMP_REDUCTION_NEQV
                | AFS_OMP_REDUCTION_IOR
                | AFS_OMP_REDUCTION_IEOR => 0,
                AFS_OMP_REDUCTION_MULTIPLY | AFS_OMP_REDUCTION_AND | AFS_OMP_REDUCTION_EQV => 1,
                AFS_OMP_REDUCTION_IAND => -1,
                AFS_OMP_REDUCTION_MAX => <$ty>::MIN,
                AFS_OMP_REDUCTION_MIN => <$ty>::MAX,
                _ => return None,
            };
            value.to_ne_bytes().to_vec()
        }};
    }
    macro_rules! real_identity {
        ($ty:ty) => {{
            let value: $ty = match operator {
                AFS_OMP_REDUCTION_ADD => 0.0,
                AFS_OMP_REDUCTION_MULTIPLY => 1.0,
                AFS_OMP_REDUCTION_MAX => <$ty>::MIN,
                AFS_OMP_REDUCTION_MIN => <$ty>::MAX,
                _ => return None,
            };
            value.to_ne_bytes().to_vec()
        }};
    }
    let identity = match kind {
        AFS_OMP_REDUCTION_KIND_I8 => scalar_identity!(i8),
        AFS_OMP_REDUCTION_KIND_I16 => scalar_identity!(i16),
        AFS_OMP_REDUCTION_KIND_I32 => scalar_identity!(i32),
        AFS_OMP_REDUCTION_KIND_I64 => scalar_identity!(i64),
        AFS_OMP_REDUCTION_KIND_F32 => real_identity!(f32),
        AFS_OMP_REDUCTION_KIND_F64 => real_identity!(f64),
        AFS_OMP_REDUCTION_KIND_C32 => {
            let real = match operator {
                AFS_OMP_REDUCTION_ADD => 0.0_f32,
                AFS_OMP_REDUCTION_MULTIPLY => 1.0_f32,
                _ => return None,
            };
            [real.to_ne_bytes(), 0.0_f32.to_ne_bytes()].concat()
        }
        AFS_OMP_REDUCTION_KIND_C64 => {
            let real = match operator {
                AFS_OMP_REDUCTION_ADD => 0.0_f64,
                AFS_OMP_REDUCTION_MULTIPLY => 1.0_f64,
                _ => return None,
            };
            [real.to_ne_bytes(), 0.0_f64.to_ne_bytes()].concat()
        }
        _ => return None,
    };
    Some(identity)
}

/// Initialize each element of one private array reduction copy.
#[no_mangle]
pub extern "C" fn afs_omp_init_reduction_array(
    operator: i32,
    kind: i32,
    storage: *mut c_void,
    count: i64,
) -> i32 {
    let Some(byte_len) = reduction_array_byte_len(kind, count) else {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    };
    let Some(identity) = reduction_array_identity(operator, kind) else {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    };
    if storage.is_null() {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let storage = unsafe { std::slice::from_raw_parts_mut(storage.cast::<u8>(), byte_len) };
    for element in storage.chunks_exact_mut(identity.len()) {
        element.copy_from_slice(&identity);
    }
    AFS_OMP_SUCCESS
}

/// Combine one contiguous private array copy across the current team.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_array(
    operator: i32,
    kind: i32,
    private_storage: *const c_void,
    original_storage: *const c_void,
    count: i64,
    result_storage: *mut c_void,
) -> i32 {
    let Some(byte_len) = reduction_array_byte_len(kind, count) else {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    };
    if private_storage.is_null()
        || original_storage.is_null()
        || result_storage.is_null()
        || !valid_reduction_array_operator(operator, kind)
    {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let context = THREAD_STATE.with(|state| {
        let state = state.borrow();
        state.teams.last().map(|team| {
            (
                team.thread_num,
                team.team_size,
                Arc::clone(&team.barrier),
                Arc::clone(&team.reduction),
            )
        })
    });
    let Some((thread_num, team_size, barrier, reduction)) = context else {
        return AFS_OMP_ERROR_NO_TEAM;
    };
    {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let team_size = usize::try_from(team_size).unwrap_or(0);
        workspace.array_slots.resize_with(team_size, Vec::new);
        let slot = &mut workspace.array_slots[thread_num as usize];
        slot.resize(byte_len, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(private_storage.cast::<u8>(), slot.as_mut_ptr(), byte_len)
        };
    }
    barrier.wait();

    if thread_num == 0 {
        let mut workspace = reduction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut combined = vec![0; byte_len];
        unsafe {
            std::ptr::copy_nonoverlapping(
                original_storage.cast::<u8>(),
                combined.as_mut_ptr(),
                byte_len,
            )
        };
        let mut status = AFS_OMP_SUCCESS;
        for slot in &workspace.array_slots {
            if !combine_reduction_arrays(operator, kind, &mut combined, slot) {
                status = AFS_OMP_ERROR_INVALID_REDUCTION;
                break;
            }
        }
        workspace.array_result = combined;
        workspace.array_status = status;
    }
    barrier.wait();

    let workspace = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if workspace.array_status != AFS_OMP_SUCCESS {
        return workspace.array_status;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            workspace.array_result.as_ptr(),
            result_storage.cast::<u8>(),
            byte_len,
        )
    };
    AFS_OMP_SUCCESS
}

/// Atomically combine one contiguous private array copy into shared storage.
#[no_mangle]
pub extern "C" fn afs_omp_reduce_array_nowait(
    operator: i32,
    kind: i32,
    private_storage: *const c_void,
    original_storage: *mut c_void,
    count: i64,
) -> i32 {
    let Some(byte_len) = reduction_array_byte_len(kind, count) else {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    };
    if private_storage.is_null()
        || original_storage.is_null()
        || !valid_reduction_array_operator(operator, kind)
    {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
    let reduction = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.reduction))
    });
    let Some(reduction) = reduction else {
        return AFS_OMP_ERROR_NO_TEAM;
    };
    let _workspace = reduction
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original =
        unsafe { std::slice::from_raw_parts_mut(original_storage.cast::<u8>(), byte_len) };
    let private = unsafe { std::slice::from_raw_parts(private_storage.cast::<u8>(), byte_len) };
    if !combine_reduction_arrays(operator, kind, original, private) {
        return AFS_OMP_ERROR_INVALID_REDUCTION;
    }
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

/// Claim the next interval for `schedule(dynamic, chunk_size)`.
///
/// The logical iteration cursor belongs to the current team and is advanced
/// while holding a short-lived mutex. Combined `parallel do` currently owns
/// one such cursor for its entire synchronous region. i128 cursor arithmetic
/// preserves every iteration of an i64-bounded Fortran loop, including ranges
/// whose logical trip count is larger than `i64::MAX`.
#[no_mangle]
pub extern "C" fn afs_omp_dynamic_bounds(
    lower: i64,
    upper: i64,
    step: i64,
    chunk_size: i64,
    first: *mut i64,
    last: *mut i64,
) -> i32 {
    if step == 0 || chunk_size <= 0 || first.is_null() || last.is_null() {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }
    let dynamic = THREAD_STATE.with(|state| {
        state
            .borrow()
            .teams
            .last()
            .map(|team| Arc::clone(&team.dynamic))
    });
    let Some(dynamic) = dynamic else {
        return -AFS_OMP_ERROR_NO_TEAM;
    };

    let lower = i128::from(lower);
    let upper = i128::from(upper);
    let step = i128::from(step);
    let iterations = loop_iteration_count(lower, upper, step);
    let chunk_size = i128::from(chunk_size);
    let (first_index, local_iterations) = {
        let mut workspace = dynamic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let first_index = workspace.next_index;
        if first_index >= iterations {
            return 0;
        }
        let local_iterations = chunk_size.min(iterations - first_index);
        workspace.next_index = first_index + local_iterations;
        (first_index, local_iterations)
    };

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

fn nonrect_inner_bounds(
    outer: i128,
    fixed_lower: i64,
    fixed_upper: i64,
    flags: i32,
) -> Option<(i128, i128)> {
    if !(1..=3).contains(&flags) {
        return None;
    }
    let lower = if flags & 1 != 0 {
        outer
    } else {
        i128::from(fixed_lower)
    };
    let upper = if flags & 2 != 0 {
        outer
    } else {
        i128::from(fixed_upper)
    };
    Some((lower, upper))
}

/// Compute the logical size of a `collapse(2)` space whose inner lower and/or
/// upper bound is the outer iteration variable itself.
///
/// Bit zero of `flags` selects an outer-dependent lower bound and bit one an
/// outer-dependent upper bound. The deliberately narrow ABI matches the
/// compiler's current, diagnosed subset of OpenMP nonrectangular bounds.
#[no_mangle]
pub extern "C" fn afs_omp_collapse2_nonrect_shape(
    outer_lower: i64,
    outer_upper: i64,
    outer_step: i64,
    inner_lower: i64,
    inner_upper: i64,
    inner_step: i64,
    flags: i32,
    outer_count: *mut i64,
    total_count: *mut i64,
) -> i32 {
    if outer_count.is_null() || total_count.is_null() {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }
    unsafe {
        *outer_count = 0;
        *total_count = 0;
    }
    if outer_step == 0 || inner_step == 0 || !(1..=3).contains(&flags) {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let outer_iterations = loop_iteration_count(
        i128::from(outer_lower),
        i128::from(outer_upper),
        i128::from(outer_step),
    );
    let Ok(outer_iterations_i64) = i64::try_from(outer_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };
    let mut total_iterations = 0_i128;
    let mut outer_index = 0_i128;
    while outer_index < outer_iterations {
        let outer = i128::from(outer_lower) + outer_index * i128::from(outer_step);
        let Some((lower, upper)) = nonrect_inner_bounds(outer, inner_lower, inner_upper, flags)
        else {
            return -AFS_OMP_ERROR_INVALID_LOOP;
        };
        let inner_iterations = loop_iteration_count(lower, upper, i128::from(inner_step));
        let Some(next_total) = total_iterations.checked_add(inner_iterations) else {
            return -AFS_OMP_ERROR_INVALID_LOOP;
        };
        total_iterations = next_total;
        outer_index += 1;
    }
    let Ok(total_iterations) = i64::try_from(total_iterations) else {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    };

    unsafe {
        *outer_count = outer_iterations_i64;
        *total_count = total_iterations;
    }
    AFS_OMP_SUCCESS
}

/// Reconstruct one logical iteration from the direct-bound nonrectangular
/// `collapse(2)` space described by `afs_omp_collapse2_nonrect_shape`.
#[no_mangle]
pub extern "C" fn afs_omp_collapse2_nonrect_indices(
    flat_index: i64,
    outer_count: i64,
    outer_lower: i64,
    outer_step: i64,
    inner_lower: i64,
    inner_upper: i64,
    inner_step: i64,
    flags: i32,
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
    if flat_index < 0
        || outer_count <= 0
        || outer_step == 0
        || inner_step == 0
        || !(1..=3).contains(&flags)
    {
        return -AFS_OMP_ERROR_INVALID_LOOP;
    }

    let mut remaining = i128::from(flat_index);
    let mut outer_index = 0_i128;
    while outer_index < i128::from(outer_count) {
        let outer = i128::from(outer_lower) + outer_index * i128::from(outer_step);
        let Some((lower, upper)) = nonrect_inner_bounds(outer, inner_lower, inner_upper, flags)
        else {
            return -AFS_OMP_ERROR_INVALID_LOOP;
        };
        let inner_iterations = loop_iteration_count(lower, upper, i128::from(inner_step));
        if remaining < inner_iterations {
            let inner = lower + remaining * i128::from(inner_step);
            let (Ok(outer), Ok(inner)) = (i64::try_from(outer), i64::try_from(inner)) else {
                return -AFS_OMP_ERROR_INVALID_LOOP;
            };
            unsafe {
                *outer_value = outer;
                *inner_value = inner;
            }
            return AFS_OMP_SUCCESS;
        }
        remaining -= inner_iterations;
        outer_index += 1;
    }
    -AFS_OMP_ERROR_INVALID_LOOP
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

    struct CriticalObservations {
        counter: AtomicUsize,
    }

    unsafe extern "C" fn critical_task(
        environment: *mut c_void,
        _thread_num: i32,
        _team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const CriticalObservations) };
        for _ in 0..500 {
            assert_eq!(
                afs_omp_critical_enter(b"OUTPUT_LOCK".as_ptr(), 11),
                AFS_OMP_SUCCESS
            );
            let value = observations.counter.load(Ordering::Relaxed);
            std::thread::yield_now();
            observations.counter.store(value + 1, Ordering::Relaxed);
            assert_eq!(
                afs_omp_critical_exit(b"output_lock".as_ptr(), 11),
                AFS_OMP_SUCCESS
            );
        }
    }

    #[test]
    fn named_critical_serializes_a_team_with_process_wide_identity() {
        reset_thread_state();
        let observations = CriticalObservations {
            counter: AtomicUsize::new(0),
        };
        assert_eq!(
            afs_omp_parallel_region(
                Some(critical_task),
                &observations as *const CriticalObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(observations.counter.load(Ordering::Relaxed), 2_000);
    }

    #[test]
    fn critical_runtime_supports_unnamed_regions_and_rejects_invalid_calls() {
        assert_eq!(afs_omp_critical_enter(std::ptr::null(), 0), AFS_OMP_SUCCESS);
        assert_eq!(afs_omp_critical_exit(b"".as_ptr(), 0), AFS_OMP_SUCCESS);
        assert_eq!(
            afs_omp_critical_enter(std::ptr::null(), 1),
            AFS_OMP_ERROR_INVALID_CRITICAL
        );
        assert_eq!(
            afs_omp_critical_enter(b"invalid".as_ptr(), -1),
            AFS_OMP_ERROR_INVALID_CRITICAL
        );
        assert_eq!(
            afs_omp_critical_exit(b"not_held".as_ptr(), 8),
            AFS_OMP_ERROR_INVALID_CRITICAL
        );
    }

    #[derive(Default)]
    struct ReductionObservations {
        values: Mutex<Vec<(i32, [i64; 8])>>,
    }

    unsafe extern "C" fn reduce_task(environment: *mut c_void, thread_num: i32, _team_size: i32) {
        let observations = unsafe { &*(environment as *const ReductionObservations) };
        let mut values = [0; 8];
        let private = i64::from(thread_num) + 1;
        let inputs = [
            (AFS_OMP_REDUCTION_ADD, private, 10),
            (AFS_OMP_REDUCTION_MULTIPLY, private, 2),
            (AFS_OMP_REDUCTION_MAX, i64::from(thread_num) - 2, -9),
            (AFS_OMP_REDUCTION_MIN, i64::from(thread_num) - 2, 9),
            (AFS_OMP_REDUCTION_AND, i64::from(thread_num != 3), 1),
            (AFS_OMP_REDUCTION_OR, i64::from(thread_num == 2), 0),
            (AFS_OMP_REDUCTION_EQV, i64::from(thread_num % 2 == 0), 1),
            (AFS_OMP_REDUCTION_NEQV, i64::from(thread_num % 2 == 0), 0),
        ];
        for (index, (operator, private, original)) in inputs.into_iter().enumerate() {
            assert_eq!(
                afs_omp_reduce_i64(operator, private, original, &mut values[index]),
                AFS_OMP_SUCCESS
            );
        }
        observations
            .values
            .lock()
            .unwrap()
            .push((thread_num, values));
    }

    #[test]
    fn integer_and_logical_reductions_combine_in_thread_order() {
        reset_thread_state();
        let observations = ReductionObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_task),
                &observations as *const ReductionObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        let mut values = observations.values.into_inner().unwrap();
        values.sort_unstable_by_key(|(thread_num, _)| *thread_num);
        assert_eq!(values.len(), 4);
        for (_, values) in values {
            assert_eq!(values, [20, 48, 1, -2, 0, 1, 1, 0]);
        }
    }

    #[derive(Default)]
    struct RealReductionObservations {
        values: Mutex<Vec<RealReductionObservation>>,
    }

    struct RealReductionObservation {
        thread_num: i32,
        values_f32: [f32; 4],
        values_f64: [f64; 4],
    }

    unsafe extern "C" fn reduce_real_task(
        environment: *mut c_void,
        thread_num: i32,
        _team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const RealReductionObservations) };
        let private_f32 = thread_num as f32 + 1.0;
        let private_f64 = f64::from(thread_num) + 1.0;
        let extrema_f32 = thread_num as f32 - 2.0;
        let extrema_f64 = f64::from(thread_num) - 2.0;
        let mut values_f32 = [0.0; 4];
        let mut values_f64 = [0.0; 4];
        let inputs_f32 = [
            (AFS_OMP_REDUCTION_ADD, private_f32, 10.0),
            (AFS_OMP_REDUCTION_MULTIPLY, private_f32, 2.0),
            (AFS_OMP_REDUCTION_MAX, extrema_f32, -9.0),
            (AFS_OMP_REDUCTION_MIN, extrema_f32, 9.0),
        ];
        for (index, (operator, private, original)) in inputs_f32.into_iter().enumerate() {
            assert_eq!(
                afs_omp_reduce_f32(operator, private, original, &mut values_f32[index]),
                AFS_OMP_SUCCESS
            );
        }
        let inputs_f64 = [
            (AFS_OMP_REDUCTION_ADD, private_f64, 10.0),
            (AFS_OMP_REDUCTION_MULTIPLY, private_f64, 2.0),
            (AFS_OMP_REDUCTION_MAX, extrema_f64, -9.0),
            (AFS_OMP_REDUCTION_MIN, extrema_f64, 9.0),
        ];
        for (index, (operator, private, original)) in inputs_f64.into_iter().enumerate() {
            assert_eq!(
                afs_omp_reduce_f64(operator, private, original, &mut values_f64[index]),
                AFS_OMP_SUCCESS
            );
        }
        observations
            .values
            .lock()
            .unwrap()
            .push(RealReductionObservation {
                thread_num,
                values_f32,
                values_f64,
            });
    }

    #[test]
    fn real_reductions_preserve_kind_precision_and_thread_order() {
        reset_thread_state();
        let observations = RealReductionObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_real_task),
                &observations as *const RealReductionObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        let mut values = observations.values.into_inner().unwrap();
        values.sort_unstable_by_key(|observation| observation.thread_num);
        assert_eq!(values.len(), 4);
        for observation in values {
            assert_eq!(observation.values_f32, [20.0, 48.0, 1.0, -2.0]);
            assert_eq!(observation.values_f64, [20.0, 48.0, 1.0, -2.0]);
        }
    }

    #[derive(Default)]
    struct ComplexReductionObservations {
        values: Mutex<Vec<ComplexReductionObservation>>,
    }

    struct ComplexReductionObservation {
        thread_num: i32,
        values_f32: [[f32; 2]; 2],
        values_f64: [[f64; 2]; 2],
    }

    unsafe extern "C" fn reduce_complex_task(
        environment: *mut c_void,
        thread_num: i32,
        _team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const ComplexReductionObservations) };
        let private_add_f32 = [thread_num as f32 + 1.0, -1.0];
        let private_add_f64 = [f64::from(thread_num) + 1.0, -1.0];
        let private_multiply_f32 = [0.0, 1.0];
        let private_multiply_f64 = [0.0, 1.0];
        let originals_f32 = [[10.0, 4.0], [2.0, -3.0]];
        let originals_f64 = [[10.0, 4.0], [2.0, -3.0]];
        let mut values_f32 = [[0.0; 2]; 2];
        let mut values_f64 = [[0.0; 2]; 2];
        assert_eq!(
            afs_omp_reduce_c32(
                AFS_OMP_REDUCTION_ADD,
                &private_add_f32,
                &originals_f32[0],
                &mut values_f32[0],
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c32(
                AFS_OMP_REDUCTION_MULTIPLY,
                &private_multiply_f32,
                &originals_f32[1],
                &mut values_f32[1],
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c64(
                AFS_OMP_REDUCTION_ADD,
                &private_add_f64,
                &originals_f64[0],
                &mut values_f64[0],
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c64(
                AFS_OMP_REDUCTION_MULTIPLY,
                &private_multiply_f64,
                &originals_f64[1],
                &mut values_f64[1],
            ),
            AFS_OMP_SUCCESS
        );
        observations
            .values
            .lock()
            .unwrap()
            .push(ComplexReductionObservation {
                thread_num,
                values_f32,
                values_f64,
            });
    }

    #[test]
    fn complex_reductions_preserve_kind_precision_and_thread_order() {
        reset_thread_state();
        let observations = ComplexReductionObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_complex_task),
                &observations as *const ComplexReductionObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        let mut values = observations.values.into_inner().unwrap();
        values.sort_unstable_by_key(|observation| observation.thread_num);
        assert_eq!(values.len(), 4);
        for observation in values {
            assert_eq!(observation.values_f32, [[20.0, 0.0], [2.0, -3.0]]);
            assert_eq!(observation.values_f64, [[20.0, 0.0], [2.0, -3.0]]);
        }
    }

    #[repr(C)]
    struct NowaitReductionObservations {
        add_i8: i8,
        multiply_i16: i16,
        maximum_i32: i32,
        minimum_i64: i64,
        logical_and: i8,
        logical_or: i8,
        real_add_f32: f32,
        real_multiply_f32: f32,
        real_maximum_f64: f64,
        real_minimum_f64: f64,
        complex_add_f32: [f32; 2],
        complex_multiply_f32: [f32; 2],
        complex_add_f64: [f64; 2],
        complex_multiply_f64: [f64; 2],
    }

    unsafe extern "C" fn reduce_nowait_task(
        environment: *mut c_void,
        thread_num: i32,
        _team_size: i32,
    ) {
        let values = environment.cast::<NowaitReductionObservations>();
        let private = i64::from(thread_num) + 1;
        let calls = [
            (
                AFS_OMP_REDUCTION_ADD,
                private,
                std::ptr::addr_of_mut!((*values).add_i8).cast(),
                1,
            ),
            (
                AFS_OMP_REDUCTION_MULTIPLY,
                private,
                std::ptr::addr_of_mut!((*values).multiply_i16).cast(),
                2,
            ),
            (
                AFS_OMP_REDUCTION_MAX,
                i64::from(thread_num) - 3,
                std::ptr::addr_of_mut!((*values).maximum_i32).cast(),
                4,
            ),
            (
                AFS_OMP_REDUCTION_MIN,
                8 - private,
                std::ptr::addr_of_mut!((*values).minimum_i64).cast(),
                8,
            ),
            (
                AFS_OMP_REDUCTION_AND,
                i64::from(thread_num != 3),
                std::ptr::addr_of_mut!((*values).logical_and).cast(),
                1,
            ),
            (
                AFS_OMP_REDUCTION_OR,
                i64::from(thread_num == 2),
                std::ptr::addr_of_mut!((*values).logical_or).cast(),
                1,
            ),
        ];
        for (operator, private, storage, storage_bytes) in calls {
            assert_eq!(
                afs_omp_reduce_i64_nowait(operator, private, storage, storage_bytes),
                AFS_OMP_SUCCESS
            );
        }
        assert_eq!(
            afs_omp_reduce_f32_nowait(
                AFS_OMP_REDUCTION_ADD,
                private as f32,
                std::ptr::addr_of_mut!((*values).real_add_f32),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_f32_nowait(
                AFS_OMP_REDUCTION_MULTIPLY,
                private as f32,
                std::ptr::addr_of_mut!((*values).real_multiply_f32),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_f64_nowait(
                AFS_OMP_REDUCTION_MAX,
                f64::from(thread_num) - 3.0,
                std::ptr::addr_of_mut!((*values).real_maximum_f64),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_f64_nowait(
                AFS_OMP_REDUCTION_MIN,
                8.0 - private as f64,
                std::ptr::addr_of_mut!((*values).real_minimum_f64),
            ),
            AFS_OMP_SUCCESS
        );
        let private_add_f32 = [private as f32, -1.0];
        let private_add_f64 = [private as f64 * 0.25, -0.5];
        let private_multiply_f32 = [0.0, 1.0];
        let private_multiply_f64 = [0.0, 1.0];
        assert_eq!(
            afs_omp_reduce_c32_nowait(
                AFS_OMP_REDUCTION_ADD,
                &private_add_f32,
                std::ptr::addr_of_mut!((*values).complex_add_f32),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c32_nowait(
                AFS_OMP_REDUCTION_MULTIPLY,
                &private_multiply_f32,
                std::ptr::addr_of_mut!((*values).complex_multiply_f32),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c64_nowait(
                AFS_OMP_REDUCTION_ADD,
                &private_add_f64,
                std::ptr::addr_of_mut!((*values).complex_add_f64),
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(
            afs_omp_reduce_c64_nowait(
                AFS_OMP_REDUCTION_MULTIPLY,
                &private_multiply_f64,
                std::ptr::addr_of_mut!((*values).complex_multiply_f64),
            ),
            AFS_OMP_SUCCESS
        );
    }

    #[test]
    fn nowait_reductions_update_typed_shared_storage_without_a_barrier() {
        reset_thread_state();
        let mut values = NowaitReductionObservations {
            add_i8: 5,
            multiply_i16: 2,
            maximum_i32: -100,
            minimum_i64: 100,
            logical_and: 1,
            logical_or: 0,
            real_add_f32: 5.0,
            real_multiply_f32: 2.0,
            real_maximum_f64: -100.0,
            real_minimum_f64: 100.0,
            complex_add_f32: [5.0, 4.0],
            complex_multiply_f32: [2.0, -3.0],
            complex_add_f64: [10.0, 2.0],
            complex_multiply_f64: [-4.0, 1.0],
        };
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_nowait_task),
                std::ptr::addr_of_mut!(values).cast(),
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(values.add_i8, 15);
        assert_eq!(values.multiply_i16, 48);
        assert_eq!(values.maximum_i32, 0);
        assert_eq!(values.minimum_i64, 4);
        assert_eq!(values.logical_and, 0);
        assert_eq!(values.logical_or, 1);
        assert_eq!(values.real_add_f32, 15.0);
        assert_eq!(values.real_multiply_f32, 48.0);
        assert_eq!(values.real_maximum_f64, 0.0);
        assert_eq!(values.real_minimum_f64, 4.0);
        assert_eq!(values.complex_add_f32, [15.0, 0.0]);
        assert_eq!(values.complex_multiply_f32, [2.0, -3.0]);
        assert_eq!(values.complex_add_f64, [12.5, 0.0]);
        assert_eq!(values.complex_multiply_f64, [-4.0, 1.0]);
    }

    #[test]
    fn array_reduction_initializer_uses_typed_openmp_identities() {
        let mut maximum = [0_i16; 3];
        assert_eq!(
            afs_omp_init_reduction_array(
                AFS_OMP_REDUCTION_MAX,
                AFS_OMP_REDUCTION_KIND_I16,
                maximum.as_mut_ptr().cast(),
                3,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(maximum, [i16::MIN; 3]);

        let mut logical_and = [0_i8; 4];
        assert_eq!(
            afs_omp_init_reduction_array(
                AFS_OMP_REDUCTION_AND,
                AFS_OMP_REDUCTION_KIND_I8,
                logical_and.as_mut_ptr().cast(),
                4,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(logical_and, [1; 4]);

        let mut complex_product = [[0.0_f32; 2]; 2];
        assert_eq!(
            afs_omp_init_reduction_array(
                AFS_OMP_REDUCTION_MULTIPLY,
                AFS_OMP_REDUCTION_KIND_C32,
                complex_product.as_mut_ptr().cast(),
                2,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(complex_product, [[1.0, 0.0]; 2]);
    }

    #[derive(Default)]
    struct ArrayReductionObservations {
        values: Mutex<Vec<ArrayReductionObservation>>,
    }

    struct ArrayReductionObservation {
        thread_num: i32,
        integer: [i8; 2],
        logical: [i16; 2],
        real: [f64; 2],
        complex: [f32; 2],
    }

    unsafe extern "C" fn reduce_array_task(
        environment: *mut c_void,
        thread_num: i32,
        _team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const ArrayReductionObservations) };
        let value = thread_num as i8 + 3;
        let private_integer = [value, -value];
        let original_integer = [120_i8, -120_i8];
        let mut integer = [0_i8; 2];
        assert_eq!(
            afs_omp_reduce_array(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_I8,
                private_integer.as_ptr().cast(),
                original_integer.as_ptr().cast(),
                2,
                integer.as_mut_ptr().cast(),
            ),
            AFS_OMP_SUCCESS
        );

        let private_logical = [1_i16, i16::from(thread_num != 3)];
        let original_logical = [1_i16, 1_i16];
        let mut logical = [0_i16; 2];
        assert_eq!(
            afs_omp_reduce_array(
                AFS_OMP_REDUCTION_AND,
                AFS_OMP_REDUCTION_KIND_I16,
                private_logical.as_ptr().cast(),
                original_logical.as_ptr().cast(),
                2,
                logical.as_mut_ptr().cast(),
            ),
            AFS_OMP_SUCCESS
        );

        let private_real = [f64::from(thread_num) + 1.0, 1.0];
        let original_real = [2.0_f64, 4.0];
        let mut real = [0.0_f64; 2];
        assert_eq!(
            afs_omp_reduce_array(
                AFS_OMP_REDUCTION_MULTIPLY,
                AFS_OMP_REDUCTION_KIND_F64,
                private_real.as_ptr().cast(),
                original_real.as_ptr().cast(),
                2,
                real.as_mut_ptr().cast(),
            ),
            AFS_OMP_SUCCESS
        );

        let private_complex = [thread_num as f32 + 1.0, -1.0];
        let original_complex = [10.0_f32, 4.0];
        let mut complex = [0.0_f32; 2];
        assert_eq!(
            afs_omp_reduce_array(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_C32,
                private_complex.as_ptr().cast(),
                original_complex.as_ptr().cast(),
                1,
                complex.as_mut_ptr().cast(),
            ),
            AFS_OMP_SUCCESS
        );
        observations
            .values
            .lock()
            .unwrap()
            .push(ArrayReductionObservation {
                thread_num,
                integer,
                logical,
                real,
                complex,
            });
    }

    #[test]
    fn array_reductions_preserve_element_kinds_and_thread_order() {
        reset_thread_state();
        let observations = ArrayReductionObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_array_task),
                &observations as *const ArrayReductionObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        let mut values = observations.values.into_inner().unwrap();
        values.sort_unstable_by_key(|observation| observation.thread_num);
        assert_eq!(values.len(), 4);
        for observation in values {
            assert_eq!(observation.integer, [-118, 118]);
            assert_eq!(observation.logical, [1, 0]);
            assert_eq!(observation.real, [48.0, 4.0]);
            assert_eq!(observation.complex, [20.0, 0.0]);
        }
    }

    struct NowaitArrayReductionObservations {
        integer: [i32; 2],
        real: [f32; 2],
        complex: [f64; 2],
    }

    unsafe extern "C" fn reduce_array_nowait_task(
        environment: *mut c_void,
        thread_num: i32,
        _team_size: i32,
    ) {
        let observations = environment.cast::<NowaitArrayReductionObservations>();
        let private_integer = [thread_num + 1, thread_num + 1];
        assert_eq!(
            afs_omp_reduce_array_nowait(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_I32,
                private_integer.as_ptr().cast(),
                std::ptr::addr_of_mut!((*observations).integer).cast(),
                2,
            ),
            AFS_OMP_SUCCESS
        );
        let private_real = [thread_num as f32 - 2.0, 10.0 - thread_num as f32];
        assert_eq!(
            afs_omp_reduce_array_nowait(
                AFS_OMP_REDUCTION_MAX,
                AFS_OMP_REDUCTION_KIND_F32,
                private_real.as_ptr().cast(),
                std::ptr::addr_of_mut!((*observations).real).cast(),
                2,
            ),
            AFS_OMP_SUCCESS
        );
        let private_complex = [0.0_f64, 1.0];
        assert_eq!(
            afs_omp_reduce_array_nowait(
                AFS_OMP_REDUCTION_MULTIPLY,
                AFS_OMP_REDUCTION_KIND_C64,
                private_complex.as_ptr().cast(),
                std::ptr::addr_of_mut!((*observations).complex).cast(),
                1,
            ),
            AFS_OMP_SUCCESS
        );
    }

    #[test]
    fn nowait_array_reductions_update_shared_storage_without_a_barrier() {
        reset_thread_state();
        let mut observations = NowaitArrayReductionObservations {
            integer: [5, -5],
            real: [-9.0, -9.0],
            complex: [2.0, -3.0],
        };
        assert_eq!(
            afs_omp_parallel_region(
                Some(reduce_array_nowait_task),
                std::ptr::addr_of_mut!(observations).cast(),
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        assert_eq!(observations.integer, [15, 5]);
        assert_eq!(observations.real, [1.0, 10.0]);
        assert_eq!(observations.complex, [2.0, -3.0]);
    }

    #[test]
    fn reduction_runtime_rejects_invalid_context_or_arguments() {
        reset_thread_state();
        let mut result = -1;
        let mut real32_result = -1.0;
        let mut real64_result = -1.0;
        let complex32_private = [1.0_f32, 2.0];
        let complex32_original = [3.0_f32, 4.0];
        let mut complex32_result = [-1.0_f32, -1.0];
        let complex64_private = [1.0_f64, 2.0];
        let complex64_original = [3.0_f64, 4.0];
        let mut complex64_result = [-1.0_f64, -1.0];
        let private_array = [1_i32, 2];
        let original_array = [3_i32, 4];
        let mut result_array = [-1_i32; 2];
        assert_eq!(
            afs_omp_reduce_i64(AFS_OMP_REDUCTION_ADD, 1, 2, &mut result),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_i64(99, 1, 2, &mut result),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_i64(AFS_OMP_REDUCTION_ADD, 1, 2, std::ptr::null_mut()),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_i64_nowait(
                AFS_OMP_REDUCTION_ADD,
                1,
                std::ptr::addr_of_mut!(result).cast(),
                4,
            ),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_i64_nowait(99, 1, std::ptr::addr_of_mut!(result).cast(), 4,),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_i64_nowait(
                AFS_OMP_REDUCTION_ADD,
                1,
                std::ptr::addr_of_mut!(result).cast(),
                3,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_i64_nowait(AFS_OMP_REDUCTION_ADD, 1, std::ptr::null_mut(), 4,),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_f32(AFS_OMP_REDUCTION_ADD, 1.0, 2.0, &mut real32_result),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_f64(99, 1.0, 2.0, &mut real64_result),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_f32(AFS_OMP_REDUCTION_ADD, 1.0, 2.0, std::ptr::null_mut()),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_f32_nowait(
                AFS_OMP_REDUCTION_ADD,
                1.0,
                std::ptr::addr_of_mut!(real32_result),
            ),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_f64_nowait(99, 1.0, std::ptr::addr_of_mut!(real64_result),),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_f64_nowait(AFS_OMP_REDUCTION_ADD, 1.0, std::ptr::null_mut()),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_c32(
                AFS_OMP_REDUCTION_ADD,
                &complex32_private,
                &complex32_original,
                &mut complex32_result,
            ),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_c64(
                AFS_OMP_REDUCTION_MAX,
                &complex64_private,
                &complex64_original,
                &mut complex64_result,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_c32(
                AFS_OMP_REDUCTION_ADD,
                std::ptr::null(),
                &complex32_original,
                &mut complex32_result,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_c64_nowait(
                AFS_OMP_REDUCTION_ADD,
                &complex64_private,
                &mut complex64_result,
            ),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_c32_nowait(
                AFS_OMP_REDUCTION_ADD,
                &complex32_private,
                std::ptr::null_mut(),
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_init_reduction_array(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_I32,
                std::ptr::null_mut(),
                2,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_init_reduction_array(
                AFS_OMP_REDUCTION_MAX,
                AFS_OMP_REDUCTION_KIND_C32,
                result_array.as_mut_ptr().cast(),
                1,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(
            afs_omp_reduce_array(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_I32,
                private_array.as_ptr().cast(),
                original_array.as_ptr().cast(),
                2,
                result_array.as_mut_ptr().cast(),
            ),
            AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_reduce_array_nowait(
                AFS_OMP_REDUCTION_ADD,
                AFS_OMP_REDUCTION_KIND_I32,
                private_array.as_ptr().cast(),
                result_array.as_mut_ptr().cast(),
                0,
            ),
            AFS_OMP_ERROR_INVALID_REDUCTION
        );
        assert_eq!(result, -1);
        assert_eq!(real32_result, -1.0);
        assert_eq!(real64_result, -1.0);
        assert_eq!(complex32_result, [-1.0, -1.0]);
        assert_eq!(complex64_result, [-1.0, -1.0]);
        assert_eq!(result_array, [-1, -1]);
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

    #[derive(Default)]
    struct DynamicObservations {
        chunks: Mutex<Vec<(i64, i64)>>,
    }

    unsafe extern "C" fn claim_dynamic_chunks(
        environment: *mut c_void,
        _thread_num: i32,
        _team_size: i32,
    ) {
        let observations = unsafe { &*(environment as *const DynamicObservations) };
        loop {
            let mut first = 0;
            let mut last = 0;
            let status = afs_omp_dynamic_bounds(10, -2, -3, 2, &mut first, &mut last);
            assert!(status >= 0, "unexpected dynamic-bounds error {status}");
            if status == 0 {
                break;
            }
            observations.chunks.lock().unwrap().push((first, last));
        }
    }

    #[test]
    fn dynamic_bounds_claim_each_chunk_once_across_the_team() {
        reset_thread_state();
        let observations = DynamicObservations::default();
        assert_eq!(
            afs_omp_parallel_region(
                Some(claim_dynamic_chunks),
                &observations as *const DynamicObservations as *mut c_void,
                1,
                4,
                0,
            ),
            AFS_OMP_SUCCESS
        );
        let mut chunks = observations.chunks.into_inner().unwrap();
        chunks.sort_unstable();
        assert_eq!(chunks, vec![(-2, -2), (4, 1), (10, 7)]);
    }

    #[test]
    fn dynamic_bounds_reject_invalid_context_and_arguments() {
        reset_thread_state();
        let mut first = 0;
        let mut last = 0;
        assert_eq!(
            afs_omp_dynamic_bounds(1, 10, 1, 1, &mut first, &mut last),
            -AFS_OMP_ERROR_NO_TEAM
        );
        assert_eq!(
            afs_omp_dynamic_bounds(1, 10, 0, 1, &mut first, &mut last),
            -AFS_OMP_ERROR_INVALID_LOOP
        );
        assert_eq!(
            afs_omp_dynamic_bounds(1, 10, 1, 0, &mut first, &mut last),
            -AFS_OMP_ERROR_INVALID_LOOP
        );
        assert_eq!(
            afs_omp_dynamic_bounds(1, 10, 1, 1, std::ptr::null_mut(), &mut last),
            -AFS_OMP_ERROR_INVALID_LOOP
        );
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

    fn nonrect_shape(
        outer: (i64, i64, i64),
        inner: (i64, i64, i64),
        flags: i32,
    ) -> Result<(i64, i64), i32> {
        let mut outer_count = -1;
        let mut total_count = -1;
        let status = afs_omp_collapse2_nonrect_shape(
            outer.0,
            outer.1,
            outer.2,
            inner.0,
            inner.1,
            inner.2,
            flags,
            &mut outer_count,
            &mut total_count,
        );
        if status == AFS_OMP_SUCCESS {
            Ok((outer_count, total_count))
        } else {
            Err(status)
        }
    }

    fn nonrect_indices(
        flat_index: i64,
        outer_count: i64,
        outer: (i64, i64),
        inner: (i64, i64, i64),
        flags: i32,
    ) -> Result<(i64, i64), i32> {
        let mut outer_value = -1;
        let mut inner_value = -1;
        let status = afs_omp_collapse2_nonrect_indices(
            flat_index,
            outer_count,
            outer.0,
            outer.1,
            inner.0,
            inner.1,
            inner.2,
            flags,
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
    fn collapse2_nonrect_direct_bounds_preserve_sequential_iteration_order() {
        assert_eq!(nonrect_shape((1, 4, 1), (0, 4, 1), 1), Ok((4, 10)));
        assert_eq!(nonrect_indices(0, 4, (1, 1), (0, 4, 1), 1), Ok((1, 1)));
        assert_eq!(nonrect_indices(3, 4, (1, 1), (0, 4, 1), 1), Ok((1, 4)));
        assert_eq!(nonrect_indices(4, 4, (1, 1), (0, 4, 1), 1), Ok((2, 2)));
        assert_eq!(nonrect_indices(9, 4, (1, 1), (0, 4, 1), 1), Ok((4, 4)));

        assert_eq!(nonrect_shape((4, 0, -2), (4, 0, -2), 2), Ok((3, 6)));
        assert_eq!(nonrect_indices(0, 3, (4, -2), (4, 0, -2), 2), Ok((4, 4)));
        assert_eq!(nonrect_indices(1, 3, (4, -2), (4, 0, -2), 2), Ok((2, 4)));
        assert_eq!(nonrect_indices(5, 3, (4, -2), (4, 0, -2), 2), Ok((0, 0)));

        assert_eq!(nonrect_shape((1, 4, 1), (0, 2, 1), 1), Ok((4, 3)));
        assert_eq!(
            nonrect_indices(3, 4, (1, 1), (0, 2, 1), 1),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(nonrect_shape((1, 3, 1), (0, 0, -7), 3), Ok((3, 3)));
    }

    #[test]
    fn collapse2_nonrect_rejects_invalid_shapes_and_indices() {
        assert_eq!(
            nonrect_shape((1, 4, 1), (1, 4, 1), 0),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            nonrect_shape((1, 4, 0), (1, 4, 1), 1),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            nonrect_shape((1, 4, 1), (1, 4, 0), 1),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            nonrect_indices(-1, 4, (1, 1), (0, 4, 1), 1),
            Err(-AFS_OMP_ERROR_INVALID_LOOP)
        );
        assert_eq!(
            nonrect_indices(0, 0, (1, 1), (0, 4, 1), 1),
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
