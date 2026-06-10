! CHECK: ok
! IR_CHECK: call @afs_modproc_defined_unary_array_result_assignment_m_flip_matrix
! IR_CHECK: call @afs_copy_array_result_to_fixed
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module defined_unary_array_result_assignment_m
  implicit none

  interface operator(.flip.)
    module procedure flip_matrix
  end interface
contains
  function flip_matrix(a) result(out)
    real, intent(in), target :: a(:, :)
    real :: out(size(a, 2), size(a, 1))
    integer :: i, j

    do j = 1, size(a, 1)
      do i = 1, size(a, 2)
        out(i, j) = a(j, i)
      end do
    end do
  end function flip_matrix
end module defined_unary_array_result_assignment_m

program defined_unary_array_result_assignment
  use defined_unary_array_result_assignment_m, only: operator(.flip.)
  implicit none

  real :: a(2, 3), fixed(3, 2)
  real, allocatable :: dyn(:, :)

  a = reshape([1.0, 2.0, 3.0, 4.0, 5.0, 6.0], [2, 3])

  fixed = .flip. a
  dyn = .flip. a

  if (abs(fixed(1, 1) - 1.0) > 1.0e-6) error stop 1
  if (abs(fixed(2, 1) - 3.0) > 1.0e-6) error stop 2
  if (abs(fixed(3, 2) - 6.0) > 1.0e-6) error stop 3
  if (.not. allocated(dyn)) error stop 4
  if (size(dyn, 1) /= 3 .or. size(dyn, 2) /= 2) error stop 5
  if (any(abs(dyn - fixed) > 1.0e-6)) error stop 6

  write(*, "(a)") "ok"
end program defined_unary_array_result_assignment
