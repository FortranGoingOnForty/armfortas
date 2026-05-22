! CHECK: ok
! IR_CHECK: call @afs_len_trim
! IR_CHECK: call @afs_compare_char
! IR_CHECK: rt_call @__afs_deallocate
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program trim_compare_temp_dealloc
  implicit none

  character(len=64) :: path
  integer :: i, count

  path = 'not-dot'
  count = 0

  do i = 1, 8
    if (trim(path) == '.' .or. trim(path) == '') then
      count = count + 1
    end if
  end do

  if (count /= 0) error stop 1
  print *, 'ok'
end program trim_compare_temp_dealloc
