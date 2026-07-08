! CHECK: max3= cd
! CHECK: minv= ab
! CHECK: padmax= a0
! CHECK: padmin= a
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar9_char_maxmin
  implicit none

  character(2) :: a
  character(2) :: b

  a = 'ab'
  b = 'a'

  if (max('ab', 'cd', 'aa') /= 'cd') error stop 1
  if (min(a, 'zz') /= 'ab') error stop 2
  if (max(b, 'a0') /= 'a0') error stop 3
  if (min(b, 'a0') /= 'a ') error stop 4

  print '(a,1x,a)', 'max3=', max('ab', 'cd', 'aa')
  print '(a,1x,a)', 'minv=', min(a, 'zz')
  print '(a,1x,a)', 'padmax=', max(b, 'a0')
  print '(a,1x,a)', 'padmin=', min(b, 'a0')
  print '(a)', 'ok'
end program ar9_char_maxmin
