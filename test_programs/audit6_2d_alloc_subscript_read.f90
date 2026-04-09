! Audit #6 probe — 2-D allocatable subscripted READ path.
!
! audit5 MAJOR-1 fixed both the write path (lower_array_store)
! and the read path (lower_array_element) to share
! compute_flat_elem_offset, which loads dim extents from the
! runtime descriptor for allocatables. The audit5_m1 test
! exercises the write path via `print *, m` (whole-array
! iteration). This sister test exercises the read path via
! per-element subscripted reads, so a regression in just the
! read side would be caught here.
!
! CHECK: 235
! CHECK: 345
! CHECK: 115
program audit6_2d_alloc_subscript_read
  integer, allocatable :: m(:,:)
  integer :: i, j
  allocate(m(3,4))
  do j = 1, 4
    do i = 1, 3
      m(i,j) = i*100 + j*10 + 5
    end do
  end do
  print *, m(2,3)  ! 235
  print *, m(3,4)  ! 345
  print *, m(1,1)  ! 115
end program
