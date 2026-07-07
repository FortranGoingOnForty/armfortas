! Regression (audit C7): the C7 loud-reject of real(16)/complex(16) must not
! touch the supported kinds. real(4), real(8), double precision, and the kind-8
! spelling real(real64) keep compiling and reporting the right kind; complex(8)
! keeps computing in double precision.
!
! (Note: kind() applied to a complex value returns 4 regardless of the actual
! component kind — a separate pre-existing bug tracked in noted_items.md — so
! this fixture proves complex(8) precision through arithmetic, not kind().)
program real_kind_supported_ok
  use iso_fortran_env, only: real32, real64
  real(4) :: a
  real(8) :: b
  real(real32) :: c
  real(real64) :: d
  double precision :: e
  complex(8) :: q
  complex(4) :: s

  a = 1.0; b = 2.0_8; c = 3.0; d = 4.0_8; e = 5.0d0
  q = (1.0_8/3.0_8, 0.0_8)
  s = (1.0/3.0, 0.0)

  print '(A,5I2)', 'rk', kind(a), kind(b), kind(c), kind(d), kind(e)
  ! CHECK: rk 4 8 4 8 8
  print '(A,F6.2)', 'db', b + d + e
  ! CHECK: db 11.00
  ! complex(8) retains full f64 precision (1/3 to 15 digits); complex(4) rounds
  print '(A,F18.15)', 'cq', real(q)
  ! CHECK: cq 0.333333333333333
  print '(A,F18.15)', 'cs', real(s)
  ! CHECK: cs 0.333333343267441
end program
