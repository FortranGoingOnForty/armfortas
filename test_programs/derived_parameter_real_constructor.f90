! CHECK: ok
! IR_CHECK: global @afs_mod_derived_parameter_real_constructor_m_c: [i8 x 24] = "speed
! REPRO_CHECK: run
module derived_parameter_real_constructor_m
  implicit none

  integer, parameter :: dp = kind(1.0d0)

  type :: constant_t
    character(len=8) :: name
    real(dp) :: value
    real :: uncertainty
  end type

  type(constant_t), parameter :: c = constant_t("speed", 299792458_dp, 0.125)
contains
  subroutine check()
    if (trim(c%name) /= "speed") error stop 1
    if (abs(c%value - 299792458.0_dp) > 0.5_dp) error stop 2
    if (abs(c%uncertainty - 0.125) > 0.000001) error stop 3
  end subroutine
end module

program derived_parameter_real_constructor
  use derived_parameter_real_constructor_m
  implicit none

  call check()
  print *, "ok"
end program
