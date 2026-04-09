! Audit #6 BLOCKING-4 — `allocate(m(0:2, 0:3))` with non-1
! lower bounds.
!
! Fixed: a new lower_alloc_bounds helper extracts the
! (lower, upper) pair from each subscript, handling both
! Element(N) → (1, N) and Range(lo, hi) → (lo, hi). The
! Stmt::Allocate arm now goes through a unified path that
! always calls afs_allocate_array (afs_allocate_1d hardcoded
! lower=1 and couldn't represent the 0:N case at all). The
! actual lower bound is now stored in the runtime descriptor,
! and compute_flat_elem_offset reads it at subscript time, so
! m(0,0) hits offset 0 and m(2,3) hits offset 11 in a 12-cell
! allocation.
!
! CHECK: 7
! CHECK: 127
! CHECK: 237
program audit6_b4_alloc_lower_bound
  integer, allocatable :: m(:,:)
  integer :: i, j
  allocate(m(0:2, 0:3))
  do j = 0, 3
    do i = 0, 2
      m(i,j) = i*100 + j*10 + 7
    end do
  end do
  print *, m(0,0)
  print *, m(1,2)
  print *, m(2,3)
end program
