! CHECK: result=10 1294365721 10
! IR_CHECK: loop_cycle_cleanup
! IR_CHECK: loop_exit_cleanup
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module loop_control_block_cleanup_m
  implicit none

  integer :: final_count = 0
  integer(kind=8) :: final_order = 0
  integer :: allocated_count = 0

  type :: tracked
    integer :: tag = 0
    integer, allocatable :: payload(:)
  contains
    final :: finish_tracked
  end type tracked
contains
  subroutine finish_tracked(value)
    type(tracked), intent(inout) :: value

    final_count = final_count + 1
    final_order = final_order * 10 + value%tag
    if (allocated(value%payload)) allocated_count = allocated_count + 1
  end subroutine finish_tracked
end module loop_control_block_cleanup_m

program loop_control_block_cleanup
  use loop_control_block_cleanup_m
  implicit none
  integer :: i

  ! CYCLE leaves the per-iteration BLOCK but retains the BLOCK enclosing the
  ! loop. The enclosing object must be finalized only after the loop finishes.
  block
    type(tracked) :: enclosing

    enclosing%tag = 9
    allocate(enclosing%payload(1))
    do i = 1, 2
      block
        type(tracked) :: iteration

        iteration%tag = i
        allocate(iteration%payload(1))
        cycle
      end block
    end do
    if (final_count /= 2 .or. final_order /= 12) error stop 1
  end block
  if (final_count /= 3 .or. final_order /= 129) error stop 2

  ! A named CYCLE targeting an outer loop leaves both nested BLOCKs. Cleanup
  ! must run from the innermost owner outward on every iteration.
  outer_cycle: do i = 1, 2
    block
      type(tracked) :: outer_iteration

      outer_iteration%tag = 2 * i + 1
      allocate(outer_iteration%payload(1))
      block
        type(tracked) :: inner_iteration

        inner_iteration%tag = 2 * i + 2
        allocate(inner_iteration%payload(1))
        cycle outer_cycle
      end block
    end block
  end do outer_cycle
  if (final_count /= 7 .or. final_order /= 1294365) error stop 3

  ! Unnamed EXIT leaves the innermost loop and its nested BLOCK.
  do
    block
      type(tracked) :: exit_value

      exit_value%tag = 7
      allocate(exit_value%payload(1))
      exit
    end block
  end do
  if (final_count /= 8 .or. final_order /= 12943657) error stop 4

  ! Named EXIT targeting the outer loop leaves both nested BLOCKs.
  outer_exit: do
    block
      type(tracked) :: outer_exit_value

      outer_exit_value%tag = 1
      allocate(outer_exit_value%payload(1))
      block
        type(tracked) :: inner_exit_value

        inner_exit_value%tag = 2
        allocate(inner_exit_value%payload(1))
        exit outer_exit
      end block
    end block
  end do outer_exit

  if (final_count /= 10) error stop 5
  if (final_order /= 1294365721_8) error stop 6
  if (allocated_count /= 10) error stop 7
  print '(a,i0,1x,i0,1x,i0)', 'result=', final_count, final_order, allocated_count
end program loop_control_block_cleanup
