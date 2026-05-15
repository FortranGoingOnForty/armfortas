! CHECK: ok
! IR_CHECK: call @afs_array_abs_complex
! IR_CHECK: fsqrt
! IR_CHECK: fcmp le
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_abs_array_scalar_compare
  implicit none

  complex(4), parameter :: tol = 1.0e-6_4
  complex(4) :: diff(2, 2)

  diff = (0.0_4, 0.0_4)
  if (.not. all(abs(diff) .le. abs(tol))) error stop 1

  print *, "ok"
end program complex_abs_array_scalar_compare
