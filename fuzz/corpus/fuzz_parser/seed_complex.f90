module m
  implicit none
  type :: point
    real :: x, y
  end type
contains
  subroutine process(arr, n)
    integer, intent(in) :: n
    type(point), intent(inout) :: arr(:)
    integer :: i
    do i = 1, n
      arr(i)%x = real(i)
      arr(i)%y = real(i) * 2.0
    end do
  end subroutine
end module
program p
  use m
  implicit none
  type(point) :: pts(10)
  call process(pts, 10)
  print *, pts(5)%x, pts(5)%y
end program
