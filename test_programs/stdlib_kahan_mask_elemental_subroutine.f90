! CHECK: ok
! IR_CHECK: elem_sub_body
! REPRO_CHECK: run
module stdlib_kahan_mask_elemental_subroutine_mod
  implicit none
  interface kernel
    module procedure kernel4
  end interface
contains
  elemental subroutine kernel4(a, s, c, mask)
    real, intent(in) :: a
    real, intent(inout) :: s
    real, intent(inout) :: c
    logical, intent(in) :: mask

    if (mask) s = s + a
    c = c + 1.0
  end subroutine
end module

program stdlib_kahan_mask_elemental_subroutine
  use stdlib_kahan_mask_elemental_subroutine_mod
  implicit none
  real :: a(5), s(5), c(5)
  logical :: mask(5)

  a = [1.0, 2.0, 3.0, 4.0, 5.0]
  s = 10.0
  c = 0.0
  mask = [.true., .false., .true., .false., .true.]

  call kernel(a, s, c, mask)

  if (abs(s(1) - 11.0) > 1.0e-6) error stop 1
  if (abs(s(2) - 10.0) > 1.0e-6) error stop 2
  if (abs(s(3) - 13.0) > 1.0e-6) error stop 3
  if (abs(s(4) - 10.0) > 1.0e-6) error stop 4
  if (abs(s(5) - 15.0) > 1.0e-6) error stop 5
  if (any(abs(c - 1.0) > 1.0e-6)) error stop 6

  print *, 'ok'
end program
