! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module generic_len_result_character_mod
  implicit none

  type :: string_type
    character(len=:), allocatable :: raw
  end type string_type

  interface len
    module procedure len_string
  end interface len

contains

  elemental function len_string(string) result(length)
    type(string_type), intent(in) :: string
    integer :: length

    if (allocated(string%raw)) then
      length = len(string%raw)
    else
      length = 0
    end if
  end function len_string

  pure function maybe(string) result(maybe_string)
    type(string_type), intent(in) :: string
    character(len=len(string)) :: maybe_string

    if (allocated(string%raw)) then
      maybe_string = string%raw
    else
      maybe_string = ''
    end if
  end function maybe

end module generic_len_result_character_mod

program generic_len_result_character
  use generic_len_result_character_mod, only : string_type, maybe, len
  implicit none

  type(string_type) :: value

  value%raw = 'pattern'
  if (len(value) /= 7) error stop
  if (maybe(value) /= 'pattern') error stop

  print *, 'ok'
end program generic_len_result_character
