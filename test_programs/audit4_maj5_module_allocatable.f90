! Audit #4 MAJOR-5 — module-level allocatable arrays are
! completely non-functional.
!
! Module deferred-shape arrays `integer, allocatable :: arr(:)`
! get total = 0 from extract_array_dims (assumed/deferred shape
! returns extent 0), and collect_module_globals' total <= 0
! guard skips emission entirely. install_globals_as_locals can't
! find the global, so subsequent references fall through and the
! program silently produces 0s for everything.
!
! Worse: even the allocate() and assignment statements get
! elided because their target name resolves to const_int 0
! before reaching lowering — the IR shows zero stores at all,
! just a print of literal 0.
!
! Module allocatables need to emit a 384-byte zero-init descriptor
! global (not a typed array) and install with allocatable=true so
! `allocate(arr(...))` finds the descriptor at runtime.
!
! Critical for fortsh: ~55 modules use allocatable shell state.
!
! XFAIL: audit MAJOR-5 (module allocatable arrays nonfunctional)
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
