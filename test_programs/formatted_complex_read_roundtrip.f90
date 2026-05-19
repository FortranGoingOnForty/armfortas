! CHECK: T T T T
! IR_CHECK: call @afs_fmt_push_real
! IR_CHECK: call @afs_fmt_read_real
! REPRO_CHECK: run
program p
  implicit none

  complex(8) :: input(2, 2), expected(2, 2)
  integer :: unit_num, i, ios

  input(1, 1) = cmplx(0.95364448718484152d-1, 0.71125635457398595d0, kind=8)
  input(1, 2) = cmplx(0.48895464513806519d0, 0.99392270028501406d0, kind=8)
  input(2, 1) = cmplx(0.61831977375696967d0, 0.14876796096338352d0, kind=8)
  input(2, 2) = cmplx(0.017005647705570115d0, 0.34722287161926957d0, kind=8)
  expected = (0.0d0, 0.0d0)

  open(newunit=unit_num, status='scratch')
  do i = 1, 2
    write(unit_num, '(*(es24.16e3,1x,es24.16e3,:,1x))', iostat=ios) input(i, :)
    if (ios /= 0) error stop 1
  end do

  rewind(unit_num)
  do i = 1, 2
    read(unit_num, '(*(es24.16e3,1x,es24.16e3,:,1x))', iostat=ios) expected(i, :)
    if (ios /= 0) error stop 2
  end do
  close(unit_num)

  if (any(input /= expected)) error stop 3
  print *, input == expected
end program p
