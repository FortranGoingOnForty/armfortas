! Regression test: multiple calls to same contained function in one block.
! The inliner must handle each call site correctly without breaking SSA.
program inline_multisite
  implicit none
  integer :: a, b, c
  a = helper(3)
  b = helper(7)
  c = helper(a + b)
  print *, a
  print *, b
  print *, c
contains
  function helper(n) result(r)
    integer, intent(in) :: n
    integer :: r
    r = n + 1
  end function
end program
! CHECK: 4
! CHECK: 8
! CHECK: 13
