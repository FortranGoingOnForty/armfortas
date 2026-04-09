! Audit #4 CRITICAL-3 — multi-dimensional slice prints now
! lower as nested column-major loops directly, bypassing the
! afs_create_section descriptor path that crashed on bare
! stack arrays.
!
! For a 3x3 column-major matrix:
!   m(1,1)=11 m(1,2)=12 m(1,3)=13
!   m(2,1)=21 m(2,2)=22 m(2,3)=23
!   m(3,1)=31 m(3,2)=32 m(3,3)=33
!
! Iteration order is column-major per F2018 §6.5.3.3 — the
! first subscript varies fastest. So `m(2:3, 1:2)` walks
! (2,1), (3,1), (2,2), (3,2) → 21, 31, 22, 32.
!
! CHECK: 11 21 31
! CHECK: 11 12 13
! CHECK: 21 31 22 32
program audit4_c3_multidim_slice_print
  integer :: m(3,3)
  integer :: i, j
  do j = 1, 3
    do i = 1, 3
      m(i, j) = i * 10 + j
    end do
  end do
  print *, m(:, 1)
  print *, m(1, :)
  print *, m(2:3, 1:2)
end program
