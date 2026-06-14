! l02a item 8: the valid neighbor of F2023 C1133. A LOCAL variable that
! is NOT referenced in the concurrent-header (bounds/step/mask) is legal —
! the constraint only forbids reading a LOCAL var in the header, where it
! is undefined. The error path is covered by gfortran-dg conditional_9.
! FLAGS: --std=f2023
program l02a_doconcurrent_local_ok
  implicit none
  integer :: i, j
  integer :: a(5)
  a = 0
  do concurrent (i = 1:5) local(j)
    j = i * 2
    a(i) = j
  end do
  print '(I0)', a(3)
  ! CHECK: 6
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l02a_doconcurrent_local_ok
