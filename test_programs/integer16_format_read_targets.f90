! CHECK: 11
! CHECK: -170141183460469231731687303715884105727
! CHECK: 33
! CHECK: 9
! CHECK: 170141183460469231731687303715884105727
! CHECK: 7
! IR_CHECK: call @afs_fmt_read_int128(
! IR_CHECK: call @afs_fmt_read_int(
! ASM_CHECK: _afs_fmt_read_int128
! ASM_CHECK: _afs_fmt_read_int
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! XFAIL(x86_64): X64-O0-001 (i128 values are not selected by the x86 backend yet)
program integer16_format_read_targets
  implicit none

  type :: box_t
    integer :: tag
    integer(16) :: wide
    integer :: tail
  end type

  integer(16) :: xs(3)
  type(box_t) :: box

  xs = [11_16, 22_16, 33_16]
  box%tag = 9
  box%wide = 44_16
  box%tail = 55

  open(10, file='afs_fmt_read_i128_targets.dat', status='replace', action='readwrite')
  write(10, '(A)') '-170141183460469231731687303715884105727  170141183460469231731687303715884105727  7'
  rewind(10)
  read(10, '(I40,1X,I40,1X,I2)') xs(2), box%wide, box%tail
  close(10)

  print *, xs(1)
  print *, xs(2)
  print *, xs(3)
  print *, box%tag
  print *, box%wide
  print *, box%tail
end program integer16_format_read_targets
