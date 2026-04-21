module m
  use iso_fortran_env, only: real64
  implicit none
  type :: value_t
    integer :: value_type = 0
    integer :: precision_kind = real64
    real(real64) :: scalar_val = 0.0_real64
    complex(real64) :: complex_val = (0.0_real64, 0.0_real64)
    real(real64), allocatable :: matrix_val(:,:)
    complex(real64), allocatable :: complex_matrix_val(:,:)
    integer :: rows = 0
    integer :: cols = 0
    logical :: is_complex_matrix = .false.
  end type

  interface assignment(=)
    module procedure assign_value
  end interface

contains

  function create_scalar(val, precision_kind) result(value)
    real(real64), intent(in) :: val
    integer, intent(in), optional :: precision_kind
    type(value_t) :: value

    value%value_type = 1
    value%precision_kind = real64
    if (present(precision_kind)) value%precision_kind = precision_kind
    value%scalar_val = val
  end function

  function create_complex(real_part, imag_part, precision_kind) result(value)
    real(real64), intent(in) :: real_part, imag_part
    integer, intent(in), optional :: precision_kind
    type(value_t) :: value

    value%value_type = 2
    value%precision_kind = real64
    if (present(precision_kind)) value%precision_kind = precision_kind
    value%complex_val = cmplx(real_part, imag_part, kind=real64)
  end function

  subroutine assign_value(lhs, rhs)
    type(value_t), intent(out) :: lhs
    type(value_t), intent(in) :: rhs

    lhs%value_type = rhs%value_type
    lhs%precision_kind = rhs%precision_kind
    lhs%scalar_val = rhs%scalar_val
    lhs%complex_val = rhs%complex_val
    lhs%rows = rhs%rows
    lhs%cols = rhs%cols
    lhs%is_complex_matrix = rhs%is_complex_matrix
  end subroutine

  function divide_values(a, b) result(c)
    type(value_t), intent(in) :: a, b
    type(value_t) :: c

    if ((a%value_type == 1 .and. b%value_type == 2) .or. &
        (a%value_type == 2 .and. b%value_type == 1)) then
      if (a%value_type == 1) then
        if (abs(b%complex_val) < epsilon(real(b%complex_val))) then
          c%value_type = 0
          return
        end if
        c%complex_val = a%scalar_val / b%complex_val
        c%value_type = 2
      else
        if (abs(b%scalar_val) < epsilon(b%scalar_val)) then
          c%value_type = 0
          return
        end if
        c = create_complex(real(a%complex_val) / b%scalar_val, &
                           aimag(a%complex_val) / b%scalar_val)
      end if
      return
    end if

    c%value_type = 0
  end function
end module

program main
  use iso_fortran_env, only: real64
  use m
  implicit none
  type(value_t) :: a, b, c

  a = create_complex(6.0_real64, 8.0_real64)
  b = create_scalar(2.0_real64)
  c = divide_values(a, b)

  print *, c%value_type, real(c%complex_val), aimag(c%complex_val)
end program
