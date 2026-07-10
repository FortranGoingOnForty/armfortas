! DATA repeat counts and implied-do object lists.
!
! CHECK: r 4 4 4
! CHECK: a 7 8 9
! CHECK: b 1 2 3 4
! CHECK: named 5 5 5
! CHECK: chars [ab][c ][xy][xy][z ]
program ar5_data_forms
  implicit none
  integer, parameter :: n = 3
  integer :: i, j
  integer :: r1, r2, r3
  integer :: a(3), b(2,2), named(3)
  character(2) :: c1, c2, carr(3)

  data r1, r2, r3 / 3*4 /
  data (a(i), i = 1, 3) / 7, 8, 9 /
  data ((b(i,j), i = 1, 2), j = 1, 2) / 1, 2, 3, 4 /
  data named / n*5 /
  data c1, c2 / 'ab', 'c' /
  data (carr(i), i = 1, 3) / 2*'xy', 'z' /

  print '(a,3(i0,1x))', 'r ', r1, r2, r3
  print '(a,3(i0,1x))', 'a ', a
  print '(a,4(i0,1x))', 'b ', b
  print '(a,3(i0,1x))', 'named ', named
  print '(11a)', 'chars [', c1, '][', c2, '][', carr(1), '][', carr(2), '][', carr(3), ']'
end program ar5_data_forms
