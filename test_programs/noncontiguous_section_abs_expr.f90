! CHECK: 1
! IR_CHECK: call @afs_create_section
! IR_CHECK: array_abs_check
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program noncontiguous_section_abs_expr
  implicit none
  real, parameter :: third = 1.0 / 3.0
  real, parameter :: twothd = 2.0 * third
  real, parameter :: rsqrt2 = 1.0 / sqrt(2.0)
  real, parameter :: rsqrt18 = 1.0 / sqrt(18.0)
  real, parameter :: sol(3,3) = reshape([rsqrt2,rsqrt18,twothd, &
                                         rsqrt2,-rsqrt18,-twothd, &
                                         0.0,4.0*rsqrt18,-third], [3,3])
  real :: vt(3,3)

  vt = 0.0
  vt(1,1) = -rsqrt2
  vt(2,1) = -rsqrt18
  vt(1,2) = -rsqrt2
  vt(2,2) = rsqrt18
  vt(1,3) = 0.0
  vt(2,3) = -4.0 * rsqrt18

  if (all(abs(abs(vt(:2,:)) - abs(sol(:2,:))) <= 1.0e-5)) then
    print *, 1
  else
    print *, 0
  end if
end program noncontiguous_section_abs_expr
