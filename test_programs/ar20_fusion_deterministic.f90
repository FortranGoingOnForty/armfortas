! Two adjacent legal loops each contain a loop-bound comparison and an
! inner conditional. Fusion must select the same comparison block in every
! fresh compiler process.
!
! CHECK: ok
program ar20_fusion_deterministic
  implicit none
  integer :: a(8), b(8), i

  a = 0
  b = 0
  do i = 1, 8
    if (i >= 1) a(i) = i * 3
  end do
  do i = 1, 8
    if (i >= 1) b(i) = a(i) + 7
  end do

  if (any(b /= [10, 13, 16, 19, 22, 25, 28, 31])) error stop 1
  print '(a)', 'ok'
end program ar20_fusion_deterministic
