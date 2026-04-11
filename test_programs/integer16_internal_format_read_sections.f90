! CHECK: 101 202
! CHECK: 606 505 404 303
! IR_CHECK: call @afs_fmt_read_int128_internal(
! ASM_CHECK: _afs_fmt_read_int128_internal
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_internal_format_read_sections
  implicit none
  character(len=64) :: buf
  integer(16) :: col(2,2), rev(2,2)

  col = 0_16
  rev = 0_16

  buf = ' 101  202'
  read(buf, '(I4,1X,I4)') col(:,2)

  buf = ' 303  404  505  606'
  read(buf, '(I4,1X,I4,1X,I4,1X,I4)') rev(2:1:-1,2:1:-1)

  print *, col(1,2), col(2,2)
  print *, rev(1,1), rev(2,1), rev(1,2), rev(2,2)
end program integer16_internal_format_read_sections
