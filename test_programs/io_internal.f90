! CHECK: 42
! IR_CHECK: call @afs_write_internal_int(
! IR_CHECK: call @afs_read_internal_int(
! ASM_CHECK: _afs_write_internal_int
! ASM_CHECK: _afs_read_internal_int
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program io_internal
  implicit none
  character(len=20) :: buf
  integer :: x

  write(buf, *) 42
  x = 0
  read(buf, *) x

  print *, x
end program io_internal
