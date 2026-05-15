! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module generic_private_rename_ascii_mod
  implicit none
contains
  elemental function lower_char(c) result(out)
    character(len=1), intent(in) :: c
    character(len=1) :: out
    integer :: k
    k = iachar(c)
    if (k >= iachar("A") .and. k <= iachar("Z")) k = k + 32
    out = char(k)
  end function

  elemental function lower_text(text) result(out)
    character(len=*), intent(in) :: text
    character(len=len(text)) :: out
    integer :: i
    do i = 1, len(text)
      out(i:i) = lower_char(text(i:i))
    end do
  end function
end module

module generic_private_rename_string_mod
  use generic_private_rename_ascii_mod, only: lower_text_ => lower_text
  implicit none
  private
  public :: string_type, assignment(=), operator(==), char, len, lower_text

  type :: string_type
    sequence
    private
    character(len=:), allocatable :: raw
  end type

  interface assignment(=)
    module procedure assign_string_char
  end interface

  interface operator(>)
    module procedure greater_string_string
  end interface

  interface operator(==)
    module procedure equal_string_string
  end interface

  interface char
    module procedure char_string
  end interface

  interface len
    module procedure len_string
  end interface

  interface lower_text
    module procedure lower_string
  end interface

contains
  elemental subroutine assign_string_char(lhs, rhs)
    type(string_type), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs
    lhs%raw = rhs
  end subroutine

  elemental function len_string(string) result(n)
    type(string_type), intent(in) :: string
    integer :: n
    if (allocated(string%raw)) then
      n = len(string%raw)
    else
      n = 0
    end if
  end function

  pure function maybe(string) result(text)
    type(string_type), intent(in) :: string
    character(len=len(string)) :: text
    if (allocated(string%raw)) then
      text = string%raw
    else
      text = ""
    end if
  end function

  elemental function lower_string(string) result(out)
    type(string_type), intent(in) :: string
    type(string_type) :: out
    out%raw = lower_text_(maybe(string))
  end function

  pure function char_string(string) result(text)
    type(string_type), intent(in) :: string
    character(len=len(string)) :: text
    text = maybe(string)
  end function

  elemental function greater_string_string(lhs, rhs) result(is_gt)
    type(string_type), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    logical :: is_gt
    is_gt = lgt(maybe(lhs), maybe(rhs))
  end function

  elemental function equal_string_string(lhs, rhs) result(is_eq)
    type(string_type), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    logical :: is_eq
    is_eq = .not.(lhs > rhs)
    if (is_eq) is_eq = .not.(rhs > lhs)
  end function
end module

program generic_private_rename_derived_result
  use generic_private_rename_string_mod, only: string_type, assignment(=), operator(==), &
    char, len, lower_text
  implicit none

  type(string_type) :: original
  type(string_type) :: expected
  type(string_type) :: lowered

  original = "To_LoWEr !$%-az09AZ"
  expected = "to_lower !$%-az09az"
  lowered = lower_text(original)

  if (len(lower_text(original)) /= 19) error stop
  if (char(lower_text(original)) /= "to_lower !$%-az09az") error stop
  if (.not.(lower_text(original) == expected)) error stop
  if (.not.(lowered == expected)) error stop

  print *, "ok"
end program
