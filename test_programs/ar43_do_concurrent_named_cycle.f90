! A named CYCLE in a multi-control DO CONCURRENT advances to the next
! control tuple, exactly like an unnamed CYCLE in the same source construct.
! It must not skip the remaining inner-control values for the current outer
! control value.
!
! FLAGS: --std=f2023
! CHECK: 11 21 31 0 0 0 13 23 33 14 24 34
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: doconc_incr
program ar43_do_concurrent_named_cycle
  implicit none
  integer :: i, j
  integer :: named_seen(3, 4), unnamed_seen(3, 4)

  named_seen = 0
  unnamed_seen = 0

tuples: do concurrent (i = 1:3, j = 1:4)
    if (j == 2) cycle tuples
    named_seen(i, j) = 10 * i + j
  end do tuples

  do concurrent (i = 1:3, j = 1:4)
    if (j == 2) cycle
    unnamed_seen(i, j) = 10 * i + j
  end do

  print *, named_seen
  if (any(named_seen /= unnamed_seen)) error stop 1
  if (count(named_seen /= 0) /= 9) error stop 2
end program ar43_do_concurrent_named_cycle
