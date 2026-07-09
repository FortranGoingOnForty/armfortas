! Runtime-bound loop remainder stores must not use an undefined address
! register after x86 linear scan.
!
! CHECK: s0= 0.0000
! CHECK: s1= 0.0000
! CHECK: a0= 81326.5000
! CHECK: a1= 1591.1250
! CHECK: a2= 1378.0000
! CHECK: a3= 213.1250
! CHECK: e= 15.0000 1596.3750
! CHECK: ic=86 0
program fz186
  implicit none
  integer :: i, n
  integer :: ic0, ic1
  real(8) :: a0(100), a1(100), a2(100), a3(100)
  real(8) :: r0, r1

  n = 100 + command_argument_count()
  do i = 1, n
    a0(i) = real(mod(i * 30, 35), 8) * 0.5_8
  end do
  do i = 1, n
    a1(i) = real(mod(i * 18, 14), 8) * 0.5_8
  end do
  do i = 1, n
    a2(i) = real(mod(i * 9, 28), 8) * 1.0_8
  end do
  do i = 1, n
    a3(i) = real(mod(i * 6, 35), 8) * 0.125_8
  end do

  r0 = 0.0_8
  r1 = 0.0_8
  ic0 = 0
  ic1 = 0
  do i = 1, n
    if (a0(i) < 0.0_8) then
      a1(i) = a2(i) + a0(i)
      ic0 = ic0 + 1
    end if
    a1(i) = a1(i) * a1(i)
    if (a0(i) > 0.0_8) then
      a1(i) = a2(i) + a1(i)
      ic0 = ic0 + 1
    end if
    a1(i) = a3(i) + a2(i)
  end do
  do i = 2, n
    a0(i) = a0(i - 1) + a1(i)
  end do

  print '(a,f16.4)', 's0=', r0
  print '(a,f16.4)', 's1=', r1
  print '(a,f16.4)', 'a0=', sum(a0)
  print '(a,f16.4)', 'a1=', sum(a1)
  print '(a,f16.4)', 'a2=', sum(a2)
  print '(a,f16.4)', 'a3=', sum(a3)
  print '(a,2f16.4)', 'e=', a0(1), a0(100)
  print '(a,i0,1x,i0)', 'ic=', ic0, ic1
end program fz186
