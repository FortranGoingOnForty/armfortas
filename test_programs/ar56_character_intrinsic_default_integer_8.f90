! With -fdefault-integer-8, character intrinsics without KIND return kind 8
! values in both semantic metadata and machine IR instead of truncating their
! runtime i64 result to the historical hard-coded i32.
!
! FLAGS: -fdefault-integer-8
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
! IR_CHECK: call @afs_index
! IR_CHECK: call @afs_scan
! IR_CHECK: call @afs_verify
program ar56_character_intrinsic_default_integer_8
  implicit none

  if (kind(index(string='banana', substring='na')) /= 8) error stop 1
  if (kind(scan(string='cabca', set='ab')) /= 8) error stop 2
  if (kind(verify(string='abXba', set='ab')) /= 8) error stop 3
  if (kind(len(string='abcd')) /= 8) error stop 4
  if (kind(len_trim(string='ab  ')) /= 8) error stop 5
  if (kind(ichar(c='A')) /= 8) error stop 6
  if (kind(iachar(c='B')) /= 8) error stop 7

  if (index(string='banana', substring='na') /= 3_8) error stop 8
  if (scan(back=.true., string='cabca', set='ab') /= 5_8) error stop 9
  if (verify(string='abXba', set='ab') /= 3_8) error stop 10
  if (len(string='abcd') /= 4_8) error stop 11
  if (len_trim(string='ab  ') /= 2_8) error stop 12
  if (ichar(c='A') /= 65_8) error stop 13
  if (iachar(c='B') /= 66_8) error stop 14

  print '(a)', 'ok'
end program ar56_character_intrinsic_default_integer_8
