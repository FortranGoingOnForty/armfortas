! CHECK: ok
! IR_CHECK: call @afs_modproc_check_like_check_logical
! REPRO_CHECK: run
module stats_like
  implicit none
  private
  public :: mean, moment

  interface mean
    module procedure mean_r3
  end interface

  interface moment
    module procedure moment_r3
  end interface
contains
  function mean_r3(x, dim) result(res)
    real, intent(in) :: x(:,:,:)
    integer, intent(in) :: dim
    real :: res(size(x, 1), size(x, 2))

    res = 1.0
    if (dim < 1) res = -1.0
  end function

  function moment_r3(x, order, dim, center) result(res)
    real, intent(in) :: x(:,:,:)
    integer, intent(in) :: order, dim
    real, intent(in) :: center(:,:)
    real :: res(size(x, 1), size(x, 2))

    res = 1.0 + 0.0 * real(order + dim) + 0.0 * center
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
    module procedure check_logical
  end interface
contains
  subroutine check_logical(error, expression)
    type(error_type), allocatable, intent(out) :: error
    logical, intent(in) :: expression

    if (.not. expression) allocate(error)
  end subroutine
end module

program stdlib_rawmoment_all_reduction_check
  use check_like, only: error_type, check
  use stats_like, only: mean, moment
  implicit none

  type(error_type), allocatable :: error
  real :: x3(2,2,3), zero3(2,2)
  integer :: order

  x3 = 1.0
  zero3 = 0.0
  order = 1

  call check(error, all(abs(moment(x3, order, dim = 3, center = zero3) - mean(x3, 3)) < 1.0e-5))
  if (allocated(error)) error stop 1

  print *, 'ok'
end program
