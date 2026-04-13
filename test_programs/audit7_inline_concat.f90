! Inline string concatenation in print statements.
! Regression test for concat SIGBUS fix.
! CHECK: abcdef
! CHECK: xyz
program test_concat
  implicit none
  print *, 'abc' // 'def'
  print *, 'x' // 'y' // 'z'
end program
