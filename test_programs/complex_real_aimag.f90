! REAL(z) and AIMAG(z) on complex values — component extraction.
! Without the complex path in lower_intrinsic, REAL(z) returned the
! whole ptr<[f32 x 2]> (IR verifier fail at O0) and AIMAG(z) fell
! through to an undefined external symbol _aimag at link time.
program complex_real_aimag
  implicit none
  complex :: c
  real :: r, i

  c = (3.5, -2.5)
  r = real(c)
  i = aimag(c)
  print *, r
  print *, i

  ! Round-trip on an arithmetic result.
  c = (1.0, 2.0) * (3.0, 4.0)
  print *, real(c), aimag(c)
end program complex_real_aimag
! CHECK:    3.5000000E0
! CHECK:   -2.5000000E0
! CHECK:   -5.0000000E0   1.0000000E1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
