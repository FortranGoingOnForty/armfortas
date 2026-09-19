! ARMFORTAS OpenMP 5.2 Fortran include file.
!
! This compatibility surface intentionally exposes only runtime procedures
! implemented by the bundled ARMFORTAS runtime. Additions must remain in sync
! with the intrinsic omp_lib module and runtime/src/openmp.rs.

integer, parameter :: openmp_version = 202111

integer, parameter :: omp_lock_kind = 8
integer, parameter :: omp_nest_lock_kind = 8
integer, parameter :: omp_sched_kind = 4
integer, parameter :: omp_proc_bind_kind = 4
integer, parameter :: omp_sync_hint_kind = 4
integer, parameter :: omp_lock_hint_kind = 4
integer, parameter :: omp_pause_resource_kind = 4
integer, parameter :: omp_allocator_handle_kind = 8
integer, parameter :: omp_alloctrait_key_kind = 4
integer, parameter :: omp_alloctrait_val_kind = 8
integer, parameter :: omp_memspace_handle_kind = 8
integer, parameter :: omp_depend_kind = 16
integer, parameter :: omp_event_handle_kind = 8
integer, parameter :: omp_interop_kind = 8
integer, parameter :: omp_interop_fr_kind = 4
integer, parameter :: omp_interop_property_kind = 4
integer, parameter :: omp_interop_rc_kind = 4

integer(omp_sched_kind), parameter :: omp_sched_static = 1
integer(omp_sched_kind), parameter :: omp_sched_dynamic = 2
integer(omp_sched_kind), parameter :: omp_sched_guided = 3
integer(omp_sched_kind), parameter :: omp_sched_auto = 4

integer(omp_proc_bind_kind), parameter :: omp_proc_bind_false = 0
integer(omp_proc_bind_kind), parameter :: omp_proc_bind_true = 1
integer(omp_proc_bind_kind), parameter :: omp_proc_bind_primary = 2
integer(omp_proc_bind_kind), parameter :: omp_proc_bind_master = 2
integer(omp_proc_bind_kind), parameter :: omp_proc_bind_close = 3
integer(omp_proc_bind_kind), parameter :: omp_proc_bind_spread = 4

integer(omp_sync_hint_kind), parameter :: omp_sync_hint_none = 0
integer(omp_sync_hint_kind), parameter :: omp_sync_hint_uncontended = 1
integer(omp_sync_hint_kind), parameter :: omp_sync_hint_contended = 2
integer(omp_sync_hint_kind), parameter :: omp_sync_hint_nonspeculative = 4
integer(omp_sync_hint_kind), parameter :: omp_sync_hint_speculative = 8

integer(omp_lock_hint_kind), parameter :: omp_lock_hint_none = 0
integer(omp_lock_hint_kind), parameter :: omp_lock_hint_uncontended = 1
integer(omp_lock_hint_kind), parameter :: omp_lock_hint_contended = 2
integer(omp_lock_hint_kind), parameter :: omp_lock_hint_nonspeculative = 4
integer(omp_lock_hint_kind), parameter :: omp_lock_hint_speculative = 8

integer(omp_pause_resource_kind), parameter :: omp_pause_soft = 1
integer(omp_pause_resource_kind), parameter :: omp_pause_hard = 2

interface
  integer function omp_get_thread_num() bind(C, name="afs_omp_get_thread_num")
  end function omp_get_thread_num

  integer function omp_get_num_threads() bind(C, name="afs_omp_get_num_threads")
  end function omp_get_num_threads

  integer function omp_get_max_threads() bind(C, name="afs_omp_get_max_threads")
  end function omp_get_max_threads

  logical function omp_in_parallel() bind(C, name="afs_omp_in_parallel")
  end function omp_in_parallel

  subroutine omp_set_num_threads(num_threads) bind(C, name="afs_omp_set_num_threads")
    integer, value :: num_threads
  end subroutine omp_set_num_threads

  double precision function omp_get_wtime() bind(C, name="afs_omp_get_wtime")
  end function omp_get_wtime

  double precision function omp_get_wtick() bind(C, name="afs_omp_get_wtick")
  end function omp_get_wtick
end interface
