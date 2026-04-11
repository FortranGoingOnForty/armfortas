! CHECK: 42
! CHECK: 7
! IR_CHECK: call @afs_fmt_read_int_internal(
! ASM_CHECK: _afs_fmt_read_int_internal
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program io_internal_format_read
  implicit none
  character(len=8) :: buf
  integer :: a, b

  buf = '  42 7'
  a = 0
  b = 0
  read(buf, '(I4,1X,I1)') a, b

  print *, a
  print *, b
end program io_internal_format_read
