! IEEE value-class inquiry and construction. Every class constant
! round-trips ieee_value -> ieee_class, and NaN detection survives the
! optimizer (the lowering goes through a runtime call, not x/=x, so a
! pass that folds a==a -> true can't break it).
!
! CHECK: T
! CHECK: T
! CHECK: T
! CHECK: T
! CHECK: T
! CHECK: T
! CHECK: nan-class T
! CHECK: inf-class T
! CHECK: support datatype T
! CHECK: support halting F
program l09_ieee_classification
  use ieee_arithmetic
  implicit none
  real(8) :: x8
  real(4) :: x4
  type(ieee_class_type) :: cls

  ! NaN is detected at every opt level (runtime call is fold-immune).
  print *, ieee_is_nan(ieee_value(1.0_8, ieee_quiet_nan))    ! T
  print *, ieee_is_nan(ieee_value(1.0_4, ieee_quiet_nan))    ! T (r4)
  print *, .not. ieee_is_nan(1.0_8)                          ! T
  print *, .not. ieee_is_finite(ieee_value(1.0_8, ieee_positive_inf))  ! T
  print *, ieee_is_finite(2.5_8)                             ! T
  print *, ieee_is_normal(1.0_8) .and. .not. ieee_is_normal(ieee_value(1.0_8, ieee_positive_denormal))  ! T

  x8 = ieee_value(1.0_8, ieee_quiet_nan)
  cls = ieee_class(x8)
  print *, 'nan-class', (cls == ieee_quiet_nan)             ! T

  x4 = ieee_value(1.0_4, ieee_positive_inf)
  print *, 'inf-class', (ieee_class(x4) == ieee_positive_inf)  ! T

  print *, 'support datatype', ieee_support_datatype(x8)    ! T
  print *, 'support halting', ieee_support_halting(x8)      ! F (honest)
end program
