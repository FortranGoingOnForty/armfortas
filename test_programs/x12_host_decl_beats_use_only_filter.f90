! Regression: a module's own host-associated declaration must win over a
! same-named entity that a USE ONLY clause filtered out. map_mod declares
! `initial_size` and also does `use node_mod, only: resize_it`; node_mod
! exports its own `initial_size` (not in the only-list). The USE-ONLY
! filter (audit MAJOR-1) flagged `initial_size` as inaccessible and the
! compile aborted, ignoring map_mod's host declaration. Host association
! takes precedence. Surfaced building fpm's tomlf_structure_ordered_map.
! x12.
!
! CHECK: k=16
module node_mod
  implicit none
  integer, parameter :: initial_size = 8
  public
contains
  subroutine resize_it()
  end subroutine
end module

module map_mod
  use node_mod, only : resize_it
  implicit none
  integer, parameter :: initial_size = 16
  private
  public :: make
contains
  subroutine make(out)
    integer, intent(out) :: out
    out = initial_size
    call resize_it()
  end subroutine
end module

program t
  use map_mod
  implicit none
  integer :: k
  call make(k)
  write(*, '(a,i0)') 'k=', k
end program t
