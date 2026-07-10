! CHECK: n,len= 3 4
! CHECK: first= aa
! CHECK: third= cc
! CHECK: pad= 32
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar9_char_array_append
  implicit none

  character(len=:), allocatable :: a(:)

  a = [character(4) :: 'aa', 'bb']
  a = [a, 'cc  ']

  if (size(a) /= 3) error stop 1
  if (len(a) /= 4) error stop 2
  if (trim(a(1)) /= 'aa') error stop 3
  if (trim(a(2)) /= 'bb') error stop 4
  if (trim(a(3)) /= 'cc') error stop 5
  if (iachar(a(3)(4:4)) /= iachar(' ')) error stop 6

  print '(a,1x,i0,1x,i0)', 'n,len=', size(a), len(a)
  print '(a,1x,a)', 'first=', trim(a(1))
  print '(a,1x,a)', 'third=', trim(a(3))
  print '(a,1x,i0)', 'pad=', iachar(a(3)(4:4))
  print '(a)', 'ok'
end program ar9_char_array_append
