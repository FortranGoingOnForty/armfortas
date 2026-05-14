! CHECK: ok
! IR_CHECK: afs_modproc_string_m_assign_string_char
! IR_CHECK: afs_modproc_string_m_eq_string_char
! IR_CHECK: afs_modproc_string_m_gt_string_char
! IR_CHECK: afs_modproc_string_m_gt_char_string
! REPRO_CHECK: run
module string_m
  implicit none

  type :: string_type
    sequence
    private
    character(len=:), allocatable :: raw
  end type string_type

  interface assignment(=)
    module procedure assign_string_char
  end interface

  interface operator(>)
    module procedure gt_string_char
    module procedure gt_char_string
  end interface

  interface operator(==)
    module procedure eq_string_char
  end interface
contains
  elemental subroutine assign_string_char(lhs, rhs)
    type(string_type), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs

    lhs%raw = rhs
  end subroutine

  elemental logical function gt_string_char(lhs, rhs) result(is_gt)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    logical :: is_gt

    if (allocated(lhs%raw)) then
      is_gt = lhs%raw > rhs
    else
      is_gt = '' > rhs
    end if
  end function

  elemental logical function gt_char_string(lhs, rhs) result(is_gt)
    character(len=*), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    logical :: is_gt

    if (allocated(rhs%raw)) then
      is_gt = lhs > rhs%raw
    else
      is_gt = lhs > ''
    end if
  end function

  elemental logical function eq_string_char(lhs, rhs) result(is_eq)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    logical :: is_eq

    is_eq = .not.(lhs > rhs)
    if (is_eq) then
      is_eq = .not.(rhs > lhs)
    end if
  end function
end module

module math_m
  use string_m, only: string_type
  implicit none

  interface swap
    module procedure swap_stt
  end interface
contains
  elemental subroutine swap_stt(lhs, rhs)
    type(string_type), intent(inout) :: lhs, rhs
    type(string_type) :: temp

    temp = lhs
    lhs = rhs
    rhs = temp
  end subroutine
end module

program main
  use string_m, only: string_type, assignment(=), operator(==)
  use math_m, only: swap
  implicit none

  type(string_type) :: x(2), y(2)

  x = ['abcde', 'fghij']
  y = ['fghij', 'abcde']

  call swap(x, y)

  if (.not. all(x == ['fghij', 'abcde'])) error stop 1
  if (.not. all(y == ['abcde', 'fghij'])) error stop 2
  call swap(x, x)
  if (.not. all(x == ['fghij', 'abcde'])) error stop 3

  print *, 'ok'
end program
