! CHECK: ok
! REPRO_CHECK: run

program rank3_middle_section_arithmetic
  implicit none

  real :: x(4,3,3)
  real :: center(4,3)
  real :: res(4,3)
  real :: expected(4,3)
  real :: d(4,3)
  integer :: a, b, c, i

  d(:,1) = [1.0, 3.0, 5.0, 7.0]
  d(:,2) = [2.0, 4.0, 6.0, 8.0]
  d(:,3) = [9.0, 10.0, 11.0, 12.0]

  do c = 1, 3
    do b = 1, 3
      do a = 1, 4
        x(a,b,c) = d(a,b) * real(2 ** (c - 1))
      end do
    end do
  end do

  do c = 1, 3
    do a = 1, 4
      center(a,c) = sum(x(a,:,c)) / 3.0
    end do
  end do

  do i = 1, 3
    do c = 1, 3
      do a = 1, 4
        expected(a,c) = x(a,i,c) - center(a,c)
      end do
    end do
    res = x(:, i, :) - center
    if (maxval(abs(res - expected)) > 1.0e-6) error stop i
  end do

  print *, 'ok'
end program
