! CHECK: 11 170141183460469231731687303715884105727 33
! CHECK: 66 -170141183460469231731687303715884105727 44
! IR_CHECK: call @afs_fmt_read_int128_internal(
! ASM_CHECK: _afs_fmt_read_int128_internal
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! XFAIL(x86_64): X64-O0-001 (i128 values are not selected by the x86 backend yet)
program integer16_internal_format_read_arrays
  implicit none
  character(len=64) :: buf
  integer(16) :: a(3), b(3)

  a = [0_16, 0_16, 0_16]
  b = [-1_16, -2_16, -3_16]
  buf = '  11  170141183460469231731687303715884105727   33'
  read(buf, '(I4,1X,I40,1X,I4)') a

  buf = '  44 -170141183460469231731687303715884105727   66'
  read(buf, '(I4,1X,I40,1X,I4)') b(3:1:-1)

  print *, a
  print *, b
end program integer16_internal_format_read_arrays
