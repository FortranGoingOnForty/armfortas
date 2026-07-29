! REAL(4) power must agree with the selected target's runtime powf.
! VOLATILE operands keep the reference calculation dynamic without imposing a
! platform-specific result bit pattern on conforming libm implementations.
program ar42_real4_powf
  use iso_fortran_env, only: int32, real32
  implicit none
  real(real32) :: base, exponent, candidate, reference
  real(real32), volatile :: runtime_base, runtime_exponent

  base = 0.43186122179031372_real32
  exponent = -0.79436987638473511_real32
  runtime_base = base
  runtime_exponent = exponent

  candidate = base ** exponent
  reference = runtime_base ** runtime_exponent
  if (transfer(candidate, 0_int32) /= transfer(reference, 0_int32)) then
    error stop 1
  end if
  print *, 'ok'
end program ar42_real4_powf
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
