! CHECK: ok
! REPRO_CHECK: run

program rank3_middle_section_real_conversion
  implicit none

  integer :: x(4,3,3)
  integer :: d(4,3)
  real(8) :: center(4,3)
  real(8) :: res(4,3)
  real(8) :: expected(4,3)
  integer :: a, b, c, i

  d(:,1) = [1, 3, 5, 7]
  d(:,2) = [2, 4, 6, 8]
  d(:,3) = [9, 10, 11, 12]

  do c = 1, 3
    do b = 1, 3
      do a = 1, 4
        x(a,b,c) = d(a,b) * (2 ** (c - 1))
      end do
    end do
  end do

  do c = 1, 3
    do a = 1, 4
      center(a,c) = real(sum(x(a,:,c)), 8) / 3.0_8
    end do
  end do

  do i = 1, 3
    do c = 1, 3
      do a = 1, 4
        expected(a,c) = real(x(a,i,c), 8) - center(a,c)
      end do
    end do
    res = real(x(:, i, :), 8) - center
    if (maxval(abs(res - expected)) > 1.0e-10_8) error stop i
  end do

  print *, 'ok'
end program
