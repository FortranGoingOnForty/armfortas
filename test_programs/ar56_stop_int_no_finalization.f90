! Integer stop-codes take the same image-termination path as a code-less
! STOP: the live marker must not be finalized before the process exits.
!
! EXIT_CODE: 17
! FILE_MISSING: ar56-stop-int-finalized.tmp
! IR_CHECK: call @afs_stop_int(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar56_stop_int_no_finalization_m
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

    open(newunit=unit, file='ar56-stop-int-finalized.tmp', &
         status='replace', action='write')
    write(unit, '(i0)') marker%value
    close(unit)
  end subroutine finish_marker

end module ar56_stop_int_no_finalization_m

program ar56_stop_int_no_finalization
  use ar56_stop_int_no_finalization_m, only: live_marker
  implicit none

  type(live_marker) :: live

  live%value = 17
  stop 17
end program ar56_stop_int_no_finalization
