! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program parameter_array_assumed_size_initializer
  use, intrinsic :: iso_fortran_env, only: real64
  implicit none

  real(real64), parameter :: xp(*) = real([1.0, 2.5, 3.5, 4.0, 5.0, 7.0, 8.5], real64)
  real(real64), parameter :: yp(*) = real([0.3, 1.1, 1.5, 2.0, 3.2, 6.6, 8.6], real64)
  real(real64) :: m(size(xp), 2)
  real(real64) :: local_b(7)

  m(:, 1) = xp**0
  m(:, 2) = xp**2
  local_b = yp

  if (size(xp) /= 7) error stop 1
  if (size(yp) /= 7) error stop 2
  if (size(m, 1) /= 7) error stop 3
  if (size(m, 2) /= 2) error stop 4
  if (size(local_b) /= 7) error stop 5

  call show_matrix(m)
  call show_vector(yp)
  call show_vector(local_b)

  write(*, "(a)") "ok"

contains
  subroutine show_matrix(a)
    real(real64), intent(in) :: a(:, :)

    if (size(a, 1) /= 7) error stop 11
    if (size(a, 2) /= 2) error stop 12
    if (a(1, 1) /= 1.0_real64) error stop 13
    if (a(7, 2) /= 72.25_real64) error stop 14
  end subroutine show_matrix

  subroutine show_vector(b)
    real(real64), intent(in) :: b(:)

    if (size(b, 1) /= 7) error stop 21
    if (abs(b(7) - 8.6_real64) > 1.0e-12_real64) error stop 22
  end subroutine show_vector
end program parameter_array_assumed_size_initializer
