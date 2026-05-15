! CHECK: ok
! REPRO_CHECK: run

program rank3_middle_section_merge_mask
  implicit none

  real(8) :: x(4,3,3)
  real(8) :: center(4,3)
  real(8) :: res(4,3)
  real(8) :: expected(4,3)
  real(8) :: n(4,3)
  real(8) :: expected_n(4,3)
  real(8) :: d(4,3)
  logical :: mask(4,3,3)
  integer :: a, b, c, i

  d(:,1) = [1._8, 3._8, 5._8, 7._8]
  d(:,2) = [2._8, 4._8, 6._8, 8._8]
  d(:,3) = [9._8, 10._8, 11._8, 12._8]

  do c = 1, 3
    do b = 1, 3
      do a = 1, 4
        x(a,b,c) = d(a,b) * real(2 ** (c - 1), 8)
      end do
    end do
  end do

  mask = x < 45._8
  center = 0._8
  expected_n = 0._8
  do c = 1, 3
    do a = 1, 4
      do b = 1, 3
        if (mask(a,b,c)) then
          center(a,c) = center(a,c) + x(a,b,c)
          expected_n(a,c) = expected_n(a,c) + 1._8
        end if
      end do
      center(a,c) = center(a,c) / expected_n(a,c)
    end do
  end do

  n = real(count(mask, 2), 8)
  if (maxval(abs(n - expected_n)) > 1.0e-10_8) error stop 1

  res = 0._8
  expected = 0._8
  do i = 1, 3
    res = res + merge((x(:, i, :) - center)**2, 0._8, mask(:, i, :))
    do c = 1, 3
      do a = 1, 4
        if (mask(a,i,c)) then
          expected(a,c) = expected(a,c) + (x(a,i,c) - center(a,c))**2
        end if
      end do
    end do
  end do

  res = res / n
  expected = expected / expected_n
  if (maxval(abs(res - expected)) > 1.0e-10_8) error stop 2

  print *, 'ok'
end program
