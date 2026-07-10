! Audit #4 MAJOR-5 — module-level allocatable arrays now work.
!
! Fixed: collect_module_globals now detects the
! `is_allocatable && array_spec.is_some()` case BEFORE the
! total<=0 deferred-shape skip and emits a 392-byte zero-init
! descriptor global. ModuleGlobalInfo gains an `allocatable`
! field; install_one_global propagates it into LocalInfo and
! types the addr as `Ptr<Array<i8, 392>>` so the runtime
! allocate/deallocate/subscript helpers see the descriptor.
!
! With this in place, the existing local-allocatable lowering
! pipeline just works for module-scope allocatables — no
! changes needed to the runtime ABI.
!
! Critical for fortsh: ~55 modules use allocatable shell state.
!
! IR-shape assertions (audit5 MIN-2, updated audit6 BLOCKING-4):
!   * The module variable must materialize as a real 392-byte
!     descriptor global (zeroed by the loader), not as a stub
!     scalar global or a never-installed local.
!   * `allocate(arr(5))` must lower to a runtime call into
!     afs_allocate_array — audit6 BLOCKING-4 unified the 1-D
!     and multi-D paths through afs_allocate_array because
!     afs_allocate_1d hardcoded lower=1 and couldn't represent
!     `allocate(a(0:N))`. The 1-D fast path is gone; everything
!     goes through afs_allocate_array now.
!
! CHECK: 10 20 30 40 50
! IR_CHECK: global @afs_mod_audit4_maj5_mod_arr: [i8 x 392] = zeroinit
! IR_CHECK: call @afs_allocate_array
! IR_NOT: call @afs_allocate_1d
program audit4_maj5_module_allocatable
  use audit4_maj5_mod
  allocate(arr(5))
  arr = [10, 20, 30, 40, 50]
  print *, arr
end program

module audit4_maj5_mod
  integer, allocatable :: arr(:)
end module audit4_maj5_mod
