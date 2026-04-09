! Two contained functions in the same compilation unit, each with a
! DO loop that mem2reg promotes into block-parameter form. Used to
! emit duplicate `do_check_1`, `do_body_2` etc. labels in the same .s
! file because block labels were not function-prefixed.
!
! CHECK: 15
! CHECK: 28
! CHECK: 24
program two_loops
  print *, sum_to(5)
  print *, sum_to(7)
  print *, prod_to(4)
contains
  function sum_to(n) result(s)
    integer, intent(in) :: n
    integer :: s, i
    s = 0
    do i = 1, n
      s = s + i
    end do
  end function

  function prod_to(n) result(p)
    integer, intent(in) :: n
    integer :: p, i
    p = 1
    do i = 1, n
      p = p * i
    end do
  end function
end program
