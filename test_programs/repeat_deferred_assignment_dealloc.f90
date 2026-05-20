! CHECK: ok
! IR_CHECK: call @afs_repeat
! IR_CHECK: call @afs_assign_char_deferred
! IR_CHECK: rt_call @__afs_deallocate
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program repeat_deferred_assignment_dealloc
  implicit none

  character(:), allocatable :: s
  integer :: i

  do i = 1, 8
    s = repeat('x', 1024)
    if (len(s) /= 1024) error stop 1
    if (s(1:1) /= 'x') error stop 2
  end do

  s = trim(s) // 'y'
  if (len(s) /= 1025) error stop 3
  if (s(1025:1025) /= 'y') error stop 4

  print *, 'ok'
end program repeat_deferred_assignment_dealloc
