! CHECK: 170141183460469231731687303715884105727
! CHECK: -170141183460469231731687303715884105727
! IR_CHECK: call @afs_fmt_push_int128(
! ASM_CHECK: _afs_fmt_push_int128
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_format
  implicit none
  integer(16) :: big, neg

  big = 170141183460469231731687303715884105727_16
  neg = -big

  write(*, '(I40)') big
  write(*, '(I40)') neg
end program integer16_format
