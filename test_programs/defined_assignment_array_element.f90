! CHECK: ok
! IR_CHECK: call @afs_modproc_m_assign_log
! REPRO_CHECK: run
module m
  implicit none

  type :: box_t
    integer, allocatable :: values(:)
  end type box_t

  interface assignment(=)
    module procedure assign_log
  end interface

contains
  subroutine assign_log(lhs, rhs)
    type(box_t), intent(out) :: lhs
    logical(1), intent(in) :: rhs(:)
    integer :: i

    allocate(lhs%values(size(rhs)))
    do i = 1, size(rhs)
      if (rhs(i)) then
        lhs%values(i) = 1
      else
        lhs%values(i) = 0
      end if
    end do
  end subroutine assign_log
end module m

program main
  use m
  implicit none

  logical(1) :: flags(4)
  type(box_t) :: boxes(0:1)
  integer :: i

  flags = .true.
  boxes(0) = flags

  if (.not. allocated(boxes(0)%values)) error stop 1
  if (size(boxes(0)%values) /= 4) error stop 2
  do i = 1, 4
    if (boxes(0)%values(i) /= 1) error stop 3
  end do

  print *, "ok"
end program main
