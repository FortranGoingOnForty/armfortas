! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_dp_parameter_zero_compare
  use, intrinsic :: iso_fortran_env, only: real64
  implicit none

  complex(real64), parameter :: zero = 0
  complex(real64) :: diag(2, 2)
  complex(real64) :: upper(2, 2)

  diag = reshape([cmplx(1., 1.), cmplx(0., 0.), &
                  cmplx(0., 0.), cmplx(4., 1.)], [2, 2])
  upper = reshape([cmplx(1., 1.), cmplx(0., 0.), &
                   cmplx(3., 1.), cmplx(4., 0.)], [2, 2])

  if (diag(2, 1) /= zero) error stop 11
  if (elem_is_nonzero(diag)) error stop 12
  if (.not. is_diag(diag)) error stop 13
  if (.not. is_upper_tri(upper)) error stop 14

  print *, "ok"

contains
  pure function elem_is_nonzero(a) result(res)
    complex(real64), intent(in) :: a(:, :)
    logical :: res
    complex(real64), parameter :: z = 0

    res = a(2, 1) /= z
  end function elem_is_nonzero

  pure function is_diag(a) result(res)
    complex(real64), intent(in) :: a(:, :)
    logical :: res
    integer :: i, j
    complex(real64), parameter :: z = 0

    res = .true.
    do j = 1, size(a, 2)
      do i = 1, size(a, 1)
        if (i /= j .and. a(i, j) /= z) then
          res = .false.
          return
        end if
      end do
    end do
  end function is_diag

  pure function is_upper_tri(a) result(res)
    complex(real64), intent(in) :: a(:, :)
    logical :: res
    integer :: i, j
    complex(real64), parameter :: z = 0

    res = .true.
    do j = 1, size(a, 2)
      do i = j + 1, size(a, 1)
        if (a(i, j) /= z) then
          res = .false.
          return
        end if
      end do
    end do
  end function is_upper_tri
end program complex_dp_parameter_zero_compare
