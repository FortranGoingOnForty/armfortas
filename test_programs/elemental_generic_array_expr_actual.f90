! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module elemental_generic_array_actual_mod
  implicit none
  private
  public :: run_elemental_generic_array_actual

  interface scale_accum
    module procedure scale_accum_r
  end interface

  interface
    module subroutine run_elemental_generic_array_actual()
    end subroutine
  end interface

contains
  elemental subroutine scale_accum_r(a, s, c)
    real, intent(in) :: a
    real, intent(inout) :: s
    real, intent(inout) :: c

    s = s + a
    c = c + 1.0
  end subroutine
end module

submodule(elemental_generic_array_actual_mod) elemental_generic_array_actual_impl
contains
  module subroutine run_elemental_generic_array_actual()
    real :: a(4)
    real :: b(4)
    real :: s(4)
    real :: c(4)

    a = [1.0, 2.0, 3.0, 4.0]
    b = [10.0, 20.0, 30.0, 40.0]
    s = 0.0
    c = 0.0

    call scale_accum(a(1:4) * b(1:4), s(1:4), c(1:4))

    if (abs(s(1) - 10.0) > 0.001) error stop 1
    if (abs(s(2) - 40.0) > 0.001) error stop 2
    if (abs(s(3) - 90.0) > 0.001) error stop 3
    if (abs(s(4) - 160.0) > 0.001) error stop 4
    if (abs(c(1) - 1.0) > 0.001) error stop 5
    if (abs(c(4) - 1.0) > 0.001) error stop 6
  end subroutine
end submodule

program elemental_generic_array_expr_actual
  use elemental_generic_array_actual_mod
  implicit none

  call run_elemental_generic_array_actual()
  print *, "ok"
end program
