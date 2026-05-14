! CHECK: ok
! IR_CHECK: call @afs_modproc_stats_like_moment_all_c2
! IR_NOT: call @afs_modproc_stats_like_moment_dim_c2
! REPRO_CHECK: run
module stats_like
  implicit none
  private
  public :: mean, moment

  interface mean
    module procedure mean_all_c2
  end interface

  interface moment
    module procedure moment_dim_c2
    module procedure moment_all_c2
  end interface
contains
  function mean_all_c2(x) result(res)
    complex, intent(in) :: x(:,:)
    complex :: res

    res = (1.0, 0.0)
  end function

  function moment_dim_c2(x, order, dim, center) result(res)
    complex, intent(in) :: x(:,:)
    integer, intent(in) :: order, dim
    complex, intent(in), optional :: center(:)
    complex :: res(size(x, 1))

    res = (1.0, 0.0) + 0.0 * real(order + dim)
  end function

  function moment_all_c2(x, order, center) result(res)
    complex, intent(in) :: x(:,:)
    integer, intent(in) :: order
    complex, intent(in), optional :: center
    complex :: res

    res = (1.0, 0.0) + 0.0 * real(order)
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

program stdlib_rawmoment_keyword_center_scalar_check
  use check_like, only: error_type, check
  use stats_like, only: mean, moment
  implicit none

  type(error_type), allocatable :: error
  complex :: x2(2,2)
  integer :: order

  x2 = (1.0, 0.0)
  order = 1

  call check(error, abs(moment(x2, order, center = (0.0, 0.0)) - mean(x2)) < 1.0e-5)
  if (allocated(error)) error stop 1

  print *, 'ok'
end program
