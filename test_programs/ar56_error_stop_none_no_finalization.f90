! Code-less ERROR STOP is abnormal image termination, not procedure or
! construct completion. Live entities must remain unfinalized.
!
! STDERR_CHECK: ERROR STOP
! EXIT_CODE: 1
! FILE_MISSING: ar56-error-stop-none-finalized.tmp
! IR_CHECK: rt_call @__afs_error_stop()
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar56_error_stop_none_no_finalization_m
  implicit none

  type :: live_marker
    integer :: value = 0
  contains
    final :: finish_marker
  end type live_marker

contains

  subroutine finish_marker(marker)
    type(live_marker), intent(inout) :: marker
    integer :: unit

    open(newunit=unit, file='ar56-error-stop-none-finalized.tmp', &
         status='replace', action='write')
    write(unit, '(i0)') marker%value
    close(unit)
  end subroutine finish_marker

end module ar56_error_stop_none_no_finalization_m

program ar56_error_stop_none_no_finalization
  use ar56_error_stop_none_no_finalization_m, only: live_marker
  implicit none

  type(live_marker) :: live

  live%value = 11
  error stop
end program ar56_error_stop_none_no_finalization
