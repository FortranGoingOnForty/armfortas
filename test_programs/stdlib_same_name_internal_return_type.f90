! CHECK: ok
! IR_CHECK: call @afs_internal_afs_modproc_m_second
! IR_CHECK: : f32
! REPRO_CHECK: run
module m
contains
  function first(a) result(s)
    integer(1), intent(in) :: a(:)
    integer(1) :: s
    s = helper(a, size(a))
  contains
    integer(1) function helper(b, n)
      integer, intent(in) :: n
      integer(1), intent(in) :: b(n)
      integer :: i
      helper = 0_1
      do i = 1, n
        helper = helper + b(i)
      end do
    end function
  end function

  function second(x) result(s)
    real, intent(in) :: x(:,:,:)
    real :: s
    s = helper(x, size(x))
  contains
    real function helper(b, n)
      integer, intent(in) :: n
      real, intent(in) :: b(n)
      helper = sum(b)
    end function
  end function
end module

program p
  use m
  implicit none
  integer(1) :: a(2)
  real :: x(2,2,2)
  a = [1_1, 2_1]
  if (first(a) /= 3_1) error stop 1
  x = 1.0
  if (abs(second(x) - 8.0) > 1.0e-6) error stop 2
  print *, 'ok'
end program
