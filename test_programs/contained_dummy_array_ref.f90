program contained_dummy_array_ref
  implicit none
  integer :: arr(2)

  arr(1) = 10
  arr(2) = 20
  call touch(arr)

  print *, arr(1)
  print *, arr(2)

contains

  subroutine touch(arr)
    implicit none
    integer, intent(inout) :: arr(2)

    arr(1) = arr(1) + 1
    arr(2) = arr(1) + arr(2)
  end subroutine touch
end program contained_dummy_array_ref

! CHECK: 11
! CHECK: 31
! IR_NOT: call @arr(
