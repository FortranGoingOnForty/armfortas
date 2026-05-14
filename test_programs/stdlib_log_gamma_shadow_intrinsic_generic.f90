! CHECK: ok
! IR_CHECK: call @afs_modproc_special_like_l_gamma_iint32
! IR_NOT: call @lgamma
! REPRO_CHECK: run
module special_like
  implicit none
  private
  public :: log_gamma

  interface log_gamma
    module procedure l_gamma_iint32
  end interface
contains
  elemental real(8) function l_gamma_iint32(z) result(res)
    integer, intent(in) :: z
    res = real(z, 8) + 0.25_8
  end function
end module

module check_like
  implicit none
  private
  public :: error_type, check

  type :: error_type
    integer :: code = 0
  end type

  interface check
    module procedure check_float_dp
  end interface
contains
  subroutine check_float_dp(error, actual, expected, message, more, thr, rel)
    type(error_type), allocatable, intent(out) :: error
    real(8), intent(in) :: actual
    real(8), intent(in) :: expected
    character(*), intent(in), optional :: message
    character(*), intent(in), optional :: more
    real(8), intent(in), optional :: thr
    logical, intent(in), optional :: rel

    if (abs(actual - expected) > 1.0e-12_8) allocate(error)
  end subroutine
end module

program stdlib_log_gamma_shadow_intrinsic_generic
  use check_like, only: error_type, check
  use special_like, only: log_gamma
  implicit none

  type(error_type), allocatable :: error
  integer :: i
  integer, parameter :: x(2) = [1, 2]
  real(8), parameter :: ans(2) = [1.25_8, 2.25_8]
  real(8), parameter :: tol = sqrt(epsilon(1.0_8))

  i = 1
  call check(error, log_gamma(x(i)), ans(i), 'integer log_gamma', thr=tol, rel=.true.)
  if (allocated(error)) error stop 1

  print *, 'ok'
end program
