! CHECK: rev= Ee5 Dd4 Cc3 Bb2 Aa1
! CHECK: func= abc def
! CHECK: dummy= ef3 cd2 ab1
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar9_char_array_rhs_assign
  implicit none

  character(3) :: c(5)
  character(3) :: r(2)
  character(3) :: d(3)

  c = [character(3) :: 'Aa1', 'Bb2', 'Cc3', 'Dd4', 'Ee5']
  c = c(5:1:-1)
  if (c(1) /= 'Ee5' .or. c(2) /= 'Dd4' .or. c(3) /= 'Cc3') error stop 1
  if (c(4) /= 'Bb2' .or. c(5) /= 'Aa1') error stop 2
  print '(a,5(1x,a))', 'rev=', c

  r = names()
  if (r(1) /= 'abc' .or. r(2) /= 'def') error stop 3
  print '(a,2(1x,a))', 'func=', r

  d = [character(3) :: 'ab1', 'cd2', 'ef3']
  call reverse_dummy(d)
  if (d(1) /= 'ef3' .or. d(2) /= 'cd2' .or. d(3) /= 'ab1') error stop 4
  print '(a,3(1x,a))', 'dummy=', d

  print '(a)', 'ok'

contains
  function names() result(n)
    character(3) :: n(2)

    n = [character(3) :: 'abc', 'def']
  end function names

  subroutine reverse_dummy(x)
    character(len=3), intent(inout) :: x(:)

    x = x(size(x):1:-1)
  end subroutine reverse_dummy
end program ar9_char_array_rhs_assign
