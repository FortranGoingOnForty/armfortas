! CHECK: ok
! IR_CHECK: call @afs_copy_array_result_to_fixed_convert
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module real_alloc_to_complex_result_m
  use iso_fortran_env, only: real32, real64
  implicit none
contains
  pure function make_eye(n) result(result)
    integer, intent(in) :: n
    real(real64), allocatable :: result(:, :)
    integer :: i

    allocate(result(n, n))
    result = 0.0_real64
    do i = 1, n
      result(i, i) = 1.0_real64
    end do
  end function make_eye
end module real_alloc_to_complex_result_m

program real_alloc_result_to_complex_fixed_array
  use iso_fortran_env, only: real32
  use real_alloc_to_complex_result_m
  implicit none

  complex(real32) :: c(3, 3)

  c = make_eye(3)
  if (abs(real(c(1, 1), kind=real32) - 1.0_real32) > 1.0e-6_real32) error stop 1
  if (abs(aimag(c(1, 1))) > 1.0e-6_real32) error stop 2
  if (abs(c(1, 2)) > 1.0e-6_real32) error stop 3

  write(*, "(a)") "ok"
end program real_alloc_result_to_complex_fixed_array
