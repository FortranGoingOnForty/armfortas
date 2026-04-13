! Test compiler_version() inquiry function from iso_fortran_env.
! CHECK: armfortas
program t
  use iso_fortran_env, only: compiler_version
  implicit none
  print *, compiler_version()
end program
