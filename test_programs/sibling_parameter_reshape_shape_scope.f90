! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module sibling_parameter_reshape_shape_scope_m
  use iso_fortran_env, only: int16
  implicit none
contains
  subroutine first()
    integer, parameter :: n = 3
    integer :: sink

    sink = n
    if (sink < 0) print *, sink
  end subroutine first

  subroutine second()
    integer, parameter :: n = 4
    integer(int16), allocatable :: a(:, :)
    integer :: i

    a = reshape([(int(i, int16), i = 1, n**2)], [n, n])
    if (size(a, 1) /= 4 .or. size(a, 2) /= 4) error stop 1
    if (a(1, 1) /= 1_int16) error stop 2
    if (a(2, 2) /= 6_int16) error stop 3
    if (a(3, 3) /= 11_int16) error stop 4
    if (a(4, 4) /= 16_int16) error stop 5
  end subroutine second
end module sibling_parameter_reshape_shape_scope_m

program sibling_parameter_reshape_shape_scope
  use sibling_parameter_reshape_shape_scope_m, only: first, second
  implicit none

  call first()
  call second()
  write(*, "(a)") "ok"
end program sibling_parameter_reshape_shape_scope
