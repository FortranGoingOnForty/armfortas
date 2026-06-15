! IEEE exception-flag get/set, and the sticky flags survive a call
! boundary (a PRINT between raise and read). The I/O runtime may itself
! raise INEXACT while formatting, but it must not clear a flag the user
! raised.
!
! CHECK: initially F
! CHECK: between
! CHECK: raised T
! CHECK: cleared F
program l09_ieee_flags
  use ieee_exceptions
  implicit none
  logical :: f

  call ieee_set_flag(ieee_overflow, .false.)
  call ieee_get_flag(ieee_overflow, f)
  print *, 'initially', f

  call ieee_set_flag(ieee_overflow, .true.)
  print *, 'between'                 ! call boundary (I/O runtime)
  call ieee_get_flag(ieee_overflow, f)
  print *, 'raised', f               ! survived the PRINT

  call ieee_set_flag(ieee_overflow, .false.)
  call ieee_get_flag(ieee_overflow, f)
  print *, 'cleared', f
end program
