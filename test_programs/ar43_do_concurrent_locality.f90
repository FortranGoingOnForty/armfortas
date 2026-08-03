! DO CONCURRENT locality creates construct entities for each iteration.
! LOCAL_INIT starts from the outside value without changing it, LOCAL does
! not alias the outside variable, SHARED keeps the outside array, and REDUCE
! combines the iteration values with the outside accumulator at termination.
!
! FLAGS: --std=f2023
! CHECK: 10 77 17
! CHECK: 111 211 112 212
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: doconc_body
! IR_CHECK: call @memcpy
program ar43_do_concurrent_locality
  implicit none
  integer :: i, j
  integer :: seed, scratch, total
  integer :: seen(2, 2)

  seed = 10
  scratch = 77
  total = 5
  seen = 0

  do concurrent (i = 1:2, j = 1:2) &
      local_init(seed) local(scratch) shared(seen) reduce(+:total) default(none)
    scratch = 100 * i + j
    seen(i, j) = seed + scratch
    seed = seed + 1
    total = total + i + j
  end do

  print *, seed, scratch, total
  print *, seen
  if (seed /= 10) error stop 1
  if (scratch /= 77) error stop 2
  if (total /= 17) error stop 3
  if (any(seen /= reshape([111, 211, 112, 212], shape(seen)))) error stop 4
end program ar43_do_concurrent_locality
