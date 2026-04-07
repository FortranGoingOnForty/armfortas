! Whole-array slice print: `print *, a(lo:hi[:step])`. Before
! the fix, slice items mis-dispatched into afs_create_section,
! which crashed with an integer overflow at runtime. The slice
! lowering also exposed a register-allocator bug where x9-x11
! were both in the allocation pool AND the spill scratch pool;
! a spill reload could clobber a freshly-computed live value.
! Audit MAJOR-4 + the spill scratch double-use bug.
!
! CHECK: 10 20 30
! CHECK: 20 30 40 50
! CHECK: 10 30 50
program slice_print
  integer :: a(5) = [10, 20, 30, 40, 50]
  print *, a(1:3)
  print *, a(2:5)
  print *, a(1:5:2)
end program
