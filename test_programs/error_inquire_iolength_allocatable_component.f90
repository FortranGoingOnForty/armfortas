! ERROR_EXPECTED: INQUIRE(IOLENGTH=) derived item has allocatable or pointer component 'name'
program error_inquire_iolength_allocatable_component
  implicit none

  type :: item_t
    integer :: value
    character(:), allocatable :: name
  end type item_t

  type(item_t) :: item
  integer :: n

  inquire(iolength=n) item
end program error_inquire_iolength_allocatable_component
