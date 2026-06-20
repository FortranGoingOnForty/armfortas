! Regression: generic dispatch must route a module/host-associated
! deferred-length allocatable CHARACTER ARRAY to the rank-1 array
! specific, not the scalar one. install_one_global built the host-
! associated LocalInfo with an empty runtime_dim_upper, so a deferred-
! shape allocatable array (no fixed bounds in info.dims) read as rank 0
! and was mis-classified as a scalar string; generic dispatch then
! matched the scalar specific (or failed). Surfaced building fpm's
! vendored M_CLI2 `insert`. x12.
!
! CHECK: array specific
module m
  implicit none
  character(len=:), allocatable, save :: keywords(:)
  interface insert
    module procedure insert_scalar
    module procedure insert_array
  end interface
contains
  subroutine doit()
    allocate(character(len=4) :: keywords(0))
    call insert(keywords, 'x', 1)
  end subroutine
  subroutine insert_scalar(item, value, place)
    character(len=:), allocatable, intent(inout) :: item
    character(len=*), intent(in) :: value
    integer, intent(in) :: place
    print *, 'scalar specific'
  end subroutine
  subroutine insert_array(list, value, place)
    character(len=:), allocatable, intent(inout) :: list(:)
    character(len=*), intent(in) :: value
    integer, intent(in) :: place
    print *, 'array specific'
  end subroutine
end module
program t
  use m
  call doit()
end program
