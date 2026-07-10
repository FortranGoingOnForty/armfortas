! CHECK: ok
! IR_CHECK: call @afs_len_trim
! IR_CHECK: call @afs_compare_char
! IR_CHECK: select_end
! IR_NOT: rt_call @__afs_allocate
! IR_NOT: rt_call @__afs_deallocate
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program select_case_trim_temp_dealloc
  implicit none

  character(len=64) :: class_name
  integer :: i, hits

  class_name = 'other'
  hits = 0

  do i = 1, 8
    if (match_class(class_name)) hits = hits + 1
  end do

  if (hits /= 0) error stop 1
  print *, 'ok'

contains

  logical function match_class(name)
    character(len=*), intent(in) :: name

    match_class = .false.
    select case (trim(name))
    case ('alpha')
      match_class = .true.
    case ('digit')
      match_class = .true.
    case ('space')
      match_class = .true.
    case default
      match_class = .false.
    end select
  end function match_class

end program select_case_trim_temp_dealloc
