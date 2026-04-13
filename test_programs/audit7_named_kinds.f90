! Named kind parameters from iso_fortran_env and iso_c_binding.
! Tests real64, c_double, int64, c_long, real32, c_float, int32, c_int.
! CHECK: r64=
! CHECK: 3.14159265358979
! CHECK: cd=
! CHECK: 2.71828182845904
! CHECK: i64= 1099511627776
! CHECK: cl= 42
! CHECK: r32=
! CHECK: cf=
! CHECK: i32= 99
! CHECK: ci= -1
program test_named_kinds
  use iso_fortran_env, only: real64, int64, int32, real32
  use iso_c_binding, only: c_double, c_long, c_int, c_float
  implicit none

  real(real64)   :: r64
  real(c_double) :: cd
  integer(int64) :: i64
  integer(c_long) :: cl
  real(real32)   :: r32
  real(c_float)  :: cf
  integer(int32) :: i32
  integer(c_int) :: ci

  r64 = 3.141592653589793d0
  cd  = 2.718281828459045d0
  i64 = 1099511627776_8    ! 2**40
  cl  = 42

  r32 = 3.14
  cf  = 2.72
  i32 = 99
  ci  = -1

  print *, 'r64=', r64
  print *, 'cd=', cd
  print *, 'i64=', i64
  print *, 'cl=', cl
  print *, 'r32=', r32
  print *, 'cf=', cf
  print *, 'i32=', i32
  print *, 'ci=', ci
end program
