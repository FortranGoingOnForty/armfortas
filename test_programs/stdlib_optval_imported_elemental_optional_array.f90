! CHECK: ok
! MULTIFILE_LINK: optval_m.f90 main.f90
! REPRO_CHECK: run
!--- file: optval_m.f90
module optval_m
  use iso_fortran_env, only: real32, real64
  implicit none
  private
  public :: optval

  interface optval
    module procedure optval_rsp
    module procedure optval_csp
    module procedure optval_cdp
    module procedure optval_character
  end interface
contains
  pure elemental function optval_rsp(x, default) result(y)
    real(real32), intent(in), optional :: x
    real(real32), intent(in) :: default
    real(real32) :: y

    if (present(x)) then
      y = x
    else
      y = default
    end if
  end function optval_rsp

  pure elemental function optval_csp(x, default) result(y)
    complex(real32), intent(in), optional :: x
    complex(real32), intent(in) :: default
    complex(real32) :: y

    if (present(x)) then
      y = x
    else
      y = default
    end if
  end function optval_csp

  pure elemental function optval_cdp(x, default) result(y)
    complex(real64), intent(in), optional :: x
    complex(real64), intent(in) :: default
    complex(real64) :: y

    if (present(x)) then
      y = x
    else
      y = default
    end if
  end function optval_cdp

  pure function optval_character(x, default) result(y)
    character(len=*), intent(in), optional :: x
    character(len=*), intent(in) :: default
    character(len=:), allocatable :: y

    if (present(x)) then
      y = x
    else
      y = default
    end if
  end function optval_character
end module optval_m
!--- file: main.f90
module test_m
  use iso_fortran_env, only: real32, real64
  use optval_m, only: optval
  implicit none
contains
  function foo_sp_arr(x) result(z)
    real(real32), dimension(2), intent(in), optional :: x
    real(real32), dimension(2) :: z

    z = optval(x, [2.0_real32, -2.0_real32])
  end function foo_sp_arr

  function foo_csp_arr(x) result(z)
    complex(real32), dimension(2), intent(in), optional :: x
    complex(real32), dimension(2) :: z

    z = optval(x, cmplx(2.0_real32, 2.0_real32, kind=real32) * [1.0_real32, -1.0_real32])
  end function foo_csp_arr

  function foo_cdp_arr(x) result(z)
    complex(real64), dimension(2), intent(in), optional :: x
    complex(real64), dimension(2) :: z

    z = optval(x, cmplx(2.0_real64, 2.0_real64, kind=real64) * [1.0_real64, -1.0_real64])
  end function foo_cdp_arr
end module test_m

program p
  use iso_fortran_env, only: real32, real64
  use test_m, only: foo_sp_arr, foo_csp_arr, foo_cdp_arr
  implicit none
  complex(real32), dimension(2) :: z1, z2
  complex(real64), dimension(2) :: z3, z4

  if (.not. all(foo_sp_arr([1.0_real32, -1.0_real32]) == [1.0_real32, -1.0_real32])) error stop 1
  if (.not. all(foo_sp_arr() == [2.0_real32, -2.0_real32])) error stop 2

  z1 = cmplx(1.0_real32, 2.0_real32, kind=real32) * [1.0_real32, -1.0_real32]
  z2 = cmplx(2.0_real32, 2.0_real32, kind=real32) * [1.0_real32, -1.0_real32]
  if (.not. all(foo_csp_arr(z1) == z1)) error stop 3
  if (.not. all(foo_csp_arr() == z2)) error stop 4

  z3 = cmplx(1.0_real64, 2.0_real64, kind=real64) * [1.0_real64, -1.0_real64]
  z4 = cmplx(2.0_real64, 2.0_real64, kind=real64) * [1.0_real64, -1.0_real64]
  if (.not. all(foo_cdp_arr(z3) == z3)) error stop 5
  if (.not. all(foo_cdp_arr() == z4)) error stop 6

  print *, 'ok'
end program p
