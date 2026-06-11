! CHECK: 170141183460469231731687303715884105727
! CHECK: 42
! IR_CHECK: call @afs_fmt_read_int128(
! IR_CHECK: call @afs_fmt_read_int(
! ASM_CHECK: _afs_fmt_read_int128
! ASM_CHECK: _afs_fmt_read_int
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_format_read
  implicit none
  integer(16) :: x
  integer :: y

  open(10, file='afs_fmt_read_i128.dat', status='replace', action='readwrite')
  write(10, '(A)') ' 170141183460469231731687303715884105727  42'
  rewind(10)
  x = 0_16
  y = 0
  read(10, '(I40,1X,I4)') x, y
  close(10)

  print *, x
  print *, y
end program integer16_format_read
