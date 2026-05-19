! CHECK: ok
! IR_CHECK: global @afs_mod_complex_module_param_reshape_exprs_mod_cs1
! IR_CHECK: 0.57706
! IR_CHECK: 4.32195
! IR_NOT: global @afs_mod_complex_module_param_reshape_exprs_mod_cs: [[f32 x 2] x 15] = zeroinit
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module complex_module_param_reshape_exprs_mod
  implicit none
  integer, parameter :: sp = kind(1.0)
  integer, parameter :: dp = kind(1.0d0)
  complex(sp), parameter :: cs1(5) = [ cmplx(0.57706_sp, 0.00000_sp, sp), &
      cmplx(0.00000_sp, 1.44065_sp, sp), &
      cmplx(1.26401_sp, 0.00000_sp, sp), &
      cmplx(0.00000_sp, 0.88833_sp, sp), &
      cmplx(1.14352_sp, 0.00000_sp, sp)]
  complex(dp), parameter :: cd1(5) = [ cmplx(0.57706_dp, 0.00000_dp, kind=dp), &
      cmplx(0.00000_dp, 1.44065_dp, kind=dp), &
      cmplx(1.26401_dp, 0.00000_dp, kind=dp), &
      cmplx(0.00000_dp, 0.88833_dp, kind=dp), &
      cmplx(1.14352_dp, 0.00000_dp, kind=dp)]
  complex(sp), parameter :: cs(5,3) = reshape([cs1, cs1*3.0_sp, cs1*1.5_sp], shape(cs))
  complex(dp), parameter :: cd(5,3) = reshape([cd1, cd1*3.0_dp, cd1*1.5_dp], shape(cd))
contains
  subroutine run()
    logical :: smask(5,3), dmask(5,3)
    smask = aimag(cs) == 0.0_sp
    dmask = aimag(cd) == 0.0_dp

    if (abs(real(cs(1,1)) - 0.57706_sp) > 1.0e-5_sp) error stop 1
    if (abs(real(cs(1,2)) - 1.73118_sp) > 1.0e-5_sp) error stop 2
    if (abs(real(cs(1,3)) - 0.86559_sp) > 1.0e-5_sp) error stop 3
    if (abs(aimag(cs(2,1)) - 1.44065_sp) > 1.0e-5_sp) error stop 4
    if (abs(aimag(cs(2,2)) - 4.32195_sp) > 1.0e-5_sp) error stop 5
    if (abs(aimag(cs(2,3)) - 2.160975_sp) > 1.0e-5_sp) error stop 6
    if (any(count(smask, 1) /= [3, 3, 3])) error stop 7
    if (any(count(smask, 2) /= [3, 0, 3, 0, 3])) error stop 8
    if (abs(real(cd(1,1)) - 0.57706_dp) > 1.0d-12) error stop 9
    if (abs(real(cd(1,2)) - 1.73118_dp) > 1.0d-12) error stop 10
    if (abs(real(cd(1,3)) - 0.86559_dp) > 1.0d-12) error stop 11
    if (abs(aimag(cd(2,1)) - 1.44065_dp) > 1.0d-12) error stop 12
    if (abs(aimag(cd(2,2)) - 4.32195_dp) > 1.0d-12) error stop 13
    if (abs(aimag(cd(2,3)) - 2.160975_dp) > 1.0d-12) error stop 14
    if (any(count(dmask, 1) /= [3, 3, 3])) error stop 15
    if (any(count(dmask, 2) /= [3, 0, 3, 0, 3])) error stop 16
    print *, 'ok'
  end subroutine
end module

program complex_module_param_reshape_exprs
  use complex_module_param_reshape_exprs_mod, only: run
  implicit none
  call run()
end program
