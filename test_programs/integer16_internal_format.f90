! CHECK: 170141183460469231731687303715884105727
! IR_CHECK: call @afs_fmt_begin_internal_ex(
! IR_CHECK: call @afs_fmt_push_int128(
! ASM_CHECK: _afs_fmt_begin_internal_ex
! ASM_CHECK: _afs_fmt_push_int128
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! XFAIL(x86_64): X64-O0-001 (i128 values are not selected by the x86 backend yet)
program integer16_internal_format
  implicit none
  character(len=64) :: buf
  integer(16) :: big

  big = 170141183460469231731687303715884105727_16
  write(buf, '(I40)') big

  print *, trim(buf)
end program integer16_internal_format
