! CHECK: ok
! IR_CHECK: direct_sum_check
! IR_CHECK: fsqrt
! IR_NOT: call @afs_array_sum_real8(
! IR_NOT: call @afs_allocate_like_with_elem_size(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program sqrt_real_argument_single_evaluation
  implicit none
  integer :: calls
  real(8) :: x(4), y

  calls = 0
  x = [1.0_8, 2.0_8, 3.0_8, 4.0_8]
  y = sqrt(mark(sum(x**2)))

  if (calls /= 1) error stop 1
  if (abs(y - sqrt(30.0_8)) > 1.0e-12_8) error stop 2
  print *, "ok"

contains

  function mark(value) result(marked)
    real(8), intent(in) :: value
    real(8) :: marked

    calls = calls + 1
    marked = value
  end function mark
end program sqrt_real_argument_single_evaluation
