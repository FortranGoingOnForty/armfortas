! CHECK: 170141183460469231731687303715884105727
! CHECK: -170141183460469231731687303715884105727
! IR_CHECK: call @afs_read_int128(
! ASM_CHECK: _afs_read_int128
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program integer16_read
  implicit none
  integer(16) :: big, x, y

  big = 170141183460469231731687303715884105727_16

  open(unit=10, file='afs_int16_read.dat', status='replace')
  write(10, *) big
  write(10, *) -big
  close(10)

  open(unit=10, file='afs_int16_read.dat', status='old')

  x = 0_16
  y = 0_16
  read(10, *) x
  read(10, *) y
  close(10)

  print *, x
  print *, y
end program integer16_read
