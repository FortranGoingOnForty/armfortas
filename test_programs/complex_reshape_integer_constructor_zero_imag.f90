! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_reshape_integer_constructor_zero_imag
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none

  call check_sp()
  call check_dp()
  print *, "ok"

contains
  subroutine check_sp()
    complex(real32) :: a(3,3)

    a = cmplx(-99.0_real32, -77.0_real32, kind=real32)
    a = reshape([3, 1, 0, &
                 1, 3, 1, &
                 0, 1, 3], shape=[3, 3])

    if (any(real(a) /= reshape([3, 1, 0, 1, 3, 1, 0, 1, 3], [3, 3]))) error stop 11
    if (any(aimag(a) /= 0.0_real32)) error stop 12
  end subroutine check_sp

  subroutine check_dp()
    complex(real64) :: a(3,3)

    a = cmplx(-99.0_real64, -77.0_real64, kind=real64)
    a = reshape([3, 1, 0, &
                 1, 3, 1, &
                 0, 1, 3], shape=[3, 3])

    if (any(real(a) /= reshape([3, 1, 0, 1, 3, 1, 0, 1, 3], [3, 3]))) error stop 21
    if (any(aimag(a) /= 0.0_real64)) error stop 22
  end subroutine check_dp
end program complex_reshape_integer_constructor_zero_imag
