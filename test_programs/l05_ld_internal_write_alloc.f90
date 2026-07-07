! Closes the l05 deferral: list-directed internal WRITE to a
! deferred-length allocatable character scalar. Previously the
! fixed-buffer path saw a len-0 view for an UNALLOCATED target and
! produced an empty string with exit 0 — the silent wrong answer
! class. Per F2023 §12.4 the target is (re)allocated to the record
! length whether it was unallocated or already allocated — it is not a
! fixed internal file (audit C12).
program l05_ld_internal_write_alloc
  implicit none
  character(len=:), allocatable :: s
  character(len=:), allocatable :: t
  integer(kind=8) :: big
  real(kind=8) :: r
  integer :: ios

  ! Unallocated target: allocated to exactly the record length.
  write(s, *) 'n =', 42
  print '(i0)', len(s)
  print '(a,a,a)', '<', s, '>'
! CHECK: 7
! CHECK: < n = 42>

  ! Mixed numeric kinds through the same collector.
  big = 123456789012345_8
  r = 2.5d0
  write(t, *) big, r
  print '(i0)', len(t)
  print '(a,a,a)', '<', t, '>'
! CHECK: 20
! CHECK: < 123456789012345 2.5>

  ! Already-allocated target: F2023 §12.4 reallocates to the new record
  ! length (' xy' = 3), it is not padded to the old length of 7.
  write(s, *) 'xy'
  print '(i0)', len(s)
  print '(a,a,a)', '<', s, '>'
! CHECK: 3
! CHECK: < xy>

  ! iostat= on the alloc path reports success.
  deallocate(s)
  write(s, *, iostat=ios) 'ok'
  print '(i0,a,a,a)', ios, '<', s, '>'
! CHECK: 0< ok>
end program l05_ld_internal_write_alloc
