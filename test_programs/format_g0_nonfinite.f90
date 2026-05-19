! CHECK: Inf
! CHECK: -Inf
! CHECK: NaN
! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: obj|repro
program format_g0_nonfinite
  implicit none
  real :: x
  character(len=8) :: s

  s = "Inf"
  read(s,*) x
  write(*,"(g0)") x

  s = "-Inf"
  read(s,*) x
  write(*,"(g0)") x

  s = "NaN"
  read(s,*) x
  write(*,"(g0)") x

  print *, "ok"
end program
