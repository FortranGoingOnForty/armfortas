! Regression: pointer-assigning the result of a character-pointer-
! returning function whose argument is polymorphic. The RHS function call
! must pass the class actual as its descriptor (so the callee's SELECT
! TYPE sees the real dynamic type); the scalar-char-pointer-target
! pointer-assignment path used a string-lowering variant that passed
! descriptor_params=None, so the actual went as a scalar data pointer,
! the callee's SELECT TYPE fell to CLASS DEFAULT, and the pointer came
! back unassociated -> empty string. Surfaced building toml-f
! (tomlf_type_keyval get_string: `val => cast_string(self%val)`); x12.
!
! CHECK: aaa
! CHECK: 3
module x12_cprc
  implicit none
  type, abstract :: gval
  end type
  type, extends(gval) :: string_value
    character(:), allocatable :: raw
  end type
contains
  function cast_string(val) result(ptr)
    class(gval), intent(in), target :: val
    character(:), pointer :: ptr
    nullify(ptr)
    select type(val)
    type is (string_value)
      ptr => val%raw
    end select
  end function
end module

program main
  use x12_cprc
  implicit none
  class(gval), allocatable, target :: v
  character(:), pointer :: dummy
  character(:), allocatable :: out

  allocate(string_value :: v)
  select type(v)
  type is (string_value)
    v%raw = "aaa"
  end select

  dummy => cast_string(v)          ! pointer-assignment of char-ptr result
  if (associated(dummy)) then
    out = dummy
    print *, out                    ! aaa
    print *, len(out)               ! 3
  else
    print *, 'UNASSOCIATED (bug)'
  end if
end program
