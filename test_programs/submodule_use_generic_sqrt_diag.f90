! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
module submodule_use_generic_diag_mod
  implicit none
  private
  public :: diag

  interface diag
    module procedure diag_real_mat
    module procedure diag_int_mat
  end interface

contains
  pure function diag_real_mat(a) result(res)
    real, intent(in) :: a(:, :)
    real :: res(min(size(a, 1), size(a, 2)))
    integer :: i

    do i = 1, size(res)
      res(i) = a(i, i)
    end do
  end function

  pure function diag_int_mat(a) result(res)
    integer, intent(in) :: a(:, :)
    integer :: res(min(size(a, 1), size(a, 2)))
    integer :: i

    do i = 1, size(res)
      res(i) = a(i, i)
    end do
  end function
end module

module submodule_use_generic_sqrt_parent
  implicit none
  private
  public :: run_submodule_use_generic_sqrt

  interface
    module subroutine run_submodule_use_generic_sqrt()
    end subroutine
  end interface
end module

submodule(submodule_use_generic_sqrt_parent) submodule_use_generic_sqrt_impl
  use submodule_use_generic_diag_mod, only: diag
contains
  module subroutine run_submodule_use_generic_sqrt()
    real :: mat(2, 2)
    real :: scale(2)

    mat = reshape([4.0, 1.0, 2.0, 9.0], [2, 2])
    scale = 1.0 / sqrt(diag(mat))

    if (abs(scale(1) - 0.5) > 0.001) error stop 1
    if (abs(scale(2) - 0.33333334) > 0.001) error stop 2
  end subroutine
end submodule

program submodule_use_generic_sqrt_diag
  use submodule_use_generic_sqrt_parent
  implicit none

  call run_submodule_use_generic_sqrt()
  print *, "ok"
end program
