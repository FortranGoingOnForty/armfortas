! Each contained proc gets hidden params only for the host vars it
! actually references — not a shared superset. `touch_x` sees only
! x; `touch_y` sees only y. If the analysis over-approximated, we'd
! either mis-route storage or hit an ABI mismatch.
! CHECK: 11 22
program host_subset
  implicit none
  integer :: x, y
  x = 10
  y = 20
  call touch_x()
  call touch_y()
  print *, x, y
contains
  subroutine touch_x()
    x = x + 1
  end subroutine
  subroutine touch_y()
    y = y + 2
  end subroutine
end program
