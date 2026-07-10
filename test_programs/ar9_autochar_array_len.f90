! CHECK: len_arr=3
! CHECK: arr1=abc
! CHECK: arr2=de
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
subroutine fill(n)
  implicit none
  integer, intent(in) :: n
  character(len=n) :: arr(2)

  arr(1) = 'abc'
  arr(2) = 'de'

  if (len(arr(1)) /= 3) error stop 1
  if (arr(1) /= 'abc') error stop 2
  if (arr(2) /= 'de ') error stop 3

  print '(a,i0)', 'len_arr=', len(arr(1))
  print '(a,a)', 'arr1=', arr(1)
  print '(a,a)', 'arr2=', arr(2)
  print '(a)', 'ok'
end subroutine fill

program ar9_autochar_array_len
  implicit none

  call fill(3)
end program ar9_autochar_array_len
