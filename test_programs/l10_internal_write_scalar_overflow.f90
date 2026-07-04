! l10: a scalar internal file has exactly one record. Values past the
! first format scan (reversion) previously vanished silently; now
! IOSTAT= reports the overflow and the target is left unmodified.
program l10_internal_write_scalar_overflow
  implicit none
  character(len=20) :: s
  integer :: ios
  s = 'sentinel'
  write(s, '(i0)', iostat=ios) 1, 2
  print '(a,l1)', 'ios_nonzero=', ios /= 0
! CHECK: ios_nonzero=T
  write(s, '(i0,1x,i0)') 7, 8
  print '(a,a,a)', '<', trim(s), '>'
! CHECK: <7 8>
end program l10_internal_write_scalar_overflow
