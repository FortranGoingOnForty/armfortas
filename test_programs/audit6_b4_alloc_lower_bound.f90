! Audit #6 BLOCKING-4 — `allocate(m(0:2, 0:3))` silently
! produces a 1-element heap allocation, then writes go out of
! bounds (heap corruption).
!
! Root cause: in lower.rs Stmt::Allocate, the multi-D path
! looks at args[i].value but only handles
! SectionSubscript::Element. When the user writes a Range like
! `0:2` the arm falls through to `b.const_i64(1)`, so every
! dim's upper bound becomes 1. The descriptor then claims
! extents (1, 1) and the runtime allocates a single element.
!
! The 1-D case has the same shape but happens to be exercised
! only by tests that pass an Element subscript, so the bug
! lurked under existing coverage.
!
! Compounding bug: even if upper bounds were honored, the
! current code hardcodes lower=1 for every dim regardless of
! the source `0:N` syntax. compute_flat_elem_offset reads the
! lower from the descriptor at subscript time, so the
! subscript math would still be wrong.
!
! Expected runtime: m(0,0)=7, m(1,2)=127, m(2,3)=237.
! Observed: m(0,0)=7 (lucky), m(1,2)=37 (wrong), m(2,3)=237.
!
! XFAIL: audit6 BLOCKING-4 (allocate Range lower bounds dropped)
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
