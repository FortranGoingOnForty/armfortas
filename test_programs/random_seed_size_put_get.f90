! CHECK: ok
! IR_CHECK: call @afs_random_seed_size
! IR_CHECK: call @afs_random_seed_put
! IR_CHECK: call @afs_random_seed_get
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program random_seed_size_put_get
  implicit none

  integer :: n
  integer, allocatable :: seed(:)
  integer, allocatable :: got(:)

  n = -1
  call random_seed(size = n)
  if (n < 1) error stop 1

  allocate(seed(n), source = 123456)
  call random_seed(put = seed)

  allocate(got(n), source = -1)
  call random_seed(get = got)
  if (got(1) /= 123456) error stop 2

  n = -1
  call random_seed(size = n)
  if (size(seed) /= n) error stop 3

  write(*, '(a)') "ok"
end program random_seed_size_put_get
