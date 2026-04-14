! Generic interface visible through a renamed USE: `use m, only: a => add`
! must let `a(1, 2)` resolve through the renamed handle. Audit-level:
! catches the case where renamed USE associations weren't surfacing
! NamedInterface symbols, leaving the call linker-unresolved.
! CHECK: 3
module add_mod
  implicit none
  interface add
    module procedure add_int
  end interface
contains
  integer function add_int(x, y)
    integer, intent(in) :: x, y
    add_int = x + y
  end function
end module
program t
  use add_mod, only: a => add
  implicit none
  print *, a(1, 2)
end program
