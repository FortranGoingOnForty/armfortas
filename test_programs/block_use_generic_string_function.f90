! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module block_use_generic_string_mod
  implicit none
  private
  public :: to_text

  interface to_text
    module procedure to_text_i
  end interface

contains
  pure function to_text_i(value) result(text)
    integer, intent(in) :: value
    character(len=5) :: text

    if (value == 3) then
      text = "three"
    else
      text = "other"
    end if
  end function
end module

program block_use_generic_string_function
  implicit none

  block
    use block_use_generic_string_mod, only: to_text
    integer :: i

    i = 3
    if ("value="//to_text(i) /= "value=three") error stop 1
  end block

  print *, "ok"
end program
