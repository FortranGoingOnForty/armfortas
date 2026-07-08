! CHECK: ok
! IR_CHECK: call @afs_adjustl
! IR_CHECK: call @afs_len_trim
! IR_CHECK: rt_call @__afs_deallocate
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program trim_adjustl_temp_dealloc
  implicit none

  character(len=8) :: text

  text = '   hi   '
  if (trim(adjustl(text)) /= 'hi') error stop 1
  print *, 'ok'
end program trim_adjustl_temp_dealloc
