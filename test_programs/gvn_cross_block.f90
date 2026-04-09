program gvn_cross_block
  implicit none
  integer :: left, right

  left = 10
  right = 4

  print *, eval_branch(left, right, 1)
  print *, eval_branch(left, right, -1)

contains

  integer function eval_branch(a, b, flag)
    implicit none
    integer, value :: a, b, flag
    integer :: tmp

    tmp = a + b
    if (flag > 0) then
      eval_branch = tmp + (a + b)
    else
      eval_branch = tmp - (a + b)
    end if
  end function eval_branch
end program gvn_cross_block

! CHECK: 28
! CHECK: 0
