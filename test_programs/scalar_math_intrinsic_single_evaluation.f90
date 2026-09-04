! CHECK: ok
! IR_CHECK: fabs
! IR_CHECK: call @exp(
! IR_CHECK: call @sin(
! IR_CHECK: call @cos(
! IR_CHECK: call @log(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program scalar_math_intrinsic_single_evaluation
  implicit none
  integer :: calls
  real(8) :: total

  calls = 0
  total = abs(mark(-2.0_8)) &
    + exp(mark(0.0_8)) &
    + sin(mark(0.0_8)) &
    + cos(mark(0.0_8)) &
    + log(mark(1.0_8))

  if (calls /= 5) error stop 1
  if (abs(total - 4.0_8) > 1.0e-12_8) error stop 2
  print *, "ok"

contains

  function mark(value) result(marked)
    real(8), intent(in) :: value
    real(8) :: marked

    calls = calls + 1
    marked = value
  end function mark
end program scalar_math_intrinsic_single_evaluation
