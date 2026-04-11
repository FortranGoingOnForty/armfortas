! CHECK: 101 202
! CHECK: 606 505 404 303
! IR_CHECK: call @afs_fmt_read_int128(
! ASM_CHECK: _afs_fmt_read_int128
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_format_read_sections
  implicit none
  integer(16) :: col(2,2), rev(2,2)

  col = 0_16
  rev = 0_16

  open(10, file='afs_fmt_read_i128_sections.dat', status='replace', action='readwrite')
  write(10, '(A)') ' 101  202'
  write(10, '(A)') ' 303  404  505  606'
  rewind(10)

  read(10, '(I4,1X,I4)') col(:,2)
  read(10, '(I4,1X,I4,1X,I4,1X,I4)') rev(2:1:-1,2:1:-1)
  close(10)

  print *, col(1,2), col(2,2)
  print *, rev(1,1), rev(2,1), rev(1,2), rev(2,2)
end program integer16_format_read_sections
