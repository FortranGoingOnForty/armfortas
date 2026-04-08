! Audit #4 MAJOR-5 — module-level allocatable arrays now work.
!
! Fixed: collect_module_globals now detects the
! `is_allocatable && array_spec.is_some()` case BEFORE the
! total<=0 deferred-shape skip and emits a 384-byte zero-init
! descriptor global. ModuleGlobalInfo gains an `allocatable`
! field; install_one_global propagates it into LocalInfo and
! types the addr as `Ptr<Array<i8, 384>>` so the runtime
! allocate/deallocate/subscript helpers see the descriptor.
!
! With this in place, the existing local-allocatable lowering
! pipeline just works for module-scope allocatables — no
! changes needed to the runtime ABI.
!
! Critical for fortsh: ~55 modules use allocatable shell state.
!
! CHECK: 10 20 30 40 50
program audit4_maj5_module_allocatable
  use audit4_maj5_mod
  allocate(arr(5))
  arr = [10, 20, 30, 40, 50]
  print *, arr
end program

module audit4_maj5_mod
  integer, allocatable :: arr(:)
end module audit4_maj5_mod
