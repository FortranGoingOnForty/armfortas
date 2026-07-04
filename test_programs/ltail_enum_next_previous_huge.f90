! Orphaned l03 deferral closed: NEXT/PREVIOUS/HUGE for enumeration
! types (F2023 16.9.118/.148/.161). HUGE is the LAST enumerator (was
! silently i32::MAX); NEXT/PREVIOUS step ordinals; STAT= reports
! out-of-range instead of erroring, leaving the value unchanged.
! Explicit-format I/O of the ordinal is pinned here too (the l05-
! owned row — it fell out of the i32-ordinal representation).
! FLAGS: --std=f2023
program ltail_enum_next_previous_huge
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  integer :: st

  c = huge(c)
  print '(i0)', int(c)
! CHECK: 3

  c = green
  c = next(c)
  print '(i0)', int(c)
! CHECK: 3
  c = previous(c)
  c = previous(c)
  print '(i0)', int(c)
! CHECK: 1

  ! STAT= form at the boundary: value unchanged, stat nonzero.
  st = -1
  c = previous(c, st)
  print '(i0,1x,i0)', int(c), st
! CHECK: 1 1
  c = next(c, st)
  print '(i0,1x,i0)', int(c), st
! CHECK: 2 0

  ! Explicit-format I/O acts on the ordinal.
  print '(i2)', c
! CHECK: 2
end program ltail_enum_next_previous_huge
