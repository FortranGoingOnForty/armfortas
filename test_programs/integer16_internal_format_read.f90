! CHECK: 170141183460469231731687303715884105727
! IR_CHECK: call @afs_fmt_read_int128_internal(
! ASM_CHECK: _afs_fmt_read_int128_internal
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_internal_format_read
  implicit none
  character(len=48) :: buf
  integer(16) :: x

  buf = ' 170141183460469231731687303715884105727'
  x = 0_16
  read(buf, '(I40)') x

  print *, x
end program integer16_internal_format_read
