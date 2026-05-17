! CHECK: ok
! IR_CHECK: call @afs_modproc_defined_unary_minus_derived_m_neg_delta
! REPRO_CHECK: run
module defined_unary_minus_derived_m
  implicit none

  type :: delta_type
    integer :: days = 0
    integer :: seconds = 0
  end type

  interface operator(-)
    module procedure neg_delta
  end interface
contains
  function make_delta(days, seconds) result(td)
    integer, intent(in) :: days
    integer, intent(in) :: seconds
    type(delta_type) :: td

    td%days = days
    td%seconds = seconds
  end function

  function neg_delta(td) result(res)
    type(delta_type), intent(in) :: td
    type(delta_type) :: res

    res%days = -td%days - 1
    res%seconds = 86400 - td%seconds
  end function
end module

program defined_unary_minus_derived
  use defined_unary_minus_derived_m
  implicit none

  type(delta_type) :: td, res

  td = make_delta(3, 21600)
  res = -td

  if (res%days /= -4) error stop 1
  if (res%seconds /= 64800) error stop 2
  print *, "ok"
end program
