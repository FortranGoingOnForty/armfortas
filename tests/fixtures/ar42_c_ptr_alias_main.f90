program cptr_alias
  use iso_c_binding, only: c_int, c_ptr, c_loc
  implicit none

  type :: pointer_holder
    type(c_ptr) :: raw
  end type pointer_holder

  interface
    subroutine overwrite_int(raw) bind(c, name="overwrite_int")
      import :: c_ptr
      type(c_ptr), value :: raw
    end subroutine overwrite_int
  end interface

  integer(c_int), target :: direct_value, saved_value, component_value
  type(c_ptr) :: saved
  type(pointer_holder) :: holder

  direct_value = 1_c_int
  saved_value = 3_c_int
  component_value = 4_c_int
  saved = c_loc(saved_value)
  holder%raw = c_loc(component_value)

  call overwrite_int(c_loc(direct_value))
  call overwrite_int(saved)
  call overwrite_int(holder%raw)

  print *, direct_value, saved_value, component_value
  if (direct_value /= 2_c_int) error stop 1
  if (saved_value /= 2_c_int) error stop 2
  if (component_value /= 2_c_int) error stop 3
end program cptr_alias
