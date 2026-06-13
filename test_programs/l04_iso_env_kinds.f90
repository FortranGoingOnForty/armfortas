! l04: F2023 iso_fortran_env kind constants — LOGICAL8/16/32/64 (kind
! = bits/8) and REAL16. armfortas has no 16-bit real, so REAL16 is the
! standard's -2 sentinel (no kind of this size, but a larger one
! exists; 16.10.2.27). The logical constants double as usable kinds.
! FLAGS: --std=f2023
! CHECK: l8 1
! CHECK: l16 2
! CHECK: l32 4
! CHECK: l64 8
! CHECK: r16 -2
! CHECK: usekind 4
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04_iso_env_kinds
  use, intrinsic :: iso_fortran_env, only: logical8, logical16, &
       logical32, logical64, real16
  implicit none
  logical(kind=logical32) :: flag

  print '(A,1X,I0)', 'l8', logical8
  print '(A,1X,I0)', 'l16', logical16
  print '(A,1X,I0)', 'l32', logical32
  print '(A,1X,I0)', 'l64', logical64
  print '(A,1X,I0)', 'r16', real16

  ! A logical constant used as a kind parameter resolves end to end.
  flag = .true.
  print '(A,1X,I0)', 'usekind', kind(flag)

  print '(A)', 'ok'
end program l04_iso_env_kinds
