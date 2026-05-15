! CHECK: ok
! IR_CHECK: call @afs_internal_afs_modproc_host_assumed_len_closure_forward_m_outer_0(%7, %6)
! IR_CHECK: func @afs_internal_afs_modproc_host_assumed_len_closure_forward_m_outer_0(%0: ptr<ptr<i8>>, %1: i64) -> void
! REPRO_CHECK: run
module host_assumed_len_closure_forward_m
  implicit none
contains
  subroutine outer(text)
    character(len=*), intent(in) :: text

    call inner()

  contains
    subroutine inner()
      if (len(text) /= 5) error stop 1
      if (text(2:4) /= 'bcd') error stop 2
    end subroutine inner
  end subroutine outer
end module host_assumed_len_closure_forward_m

program host_assumed_len_closure_forward
  use host_assumed_len_closure_forward_m
  implicit none

  call outer('abcde')
  print *, 'ok'
end program host_assumed_len_closure_forward
