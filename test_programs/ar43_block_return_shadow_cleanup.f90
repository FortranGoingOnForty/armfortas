! RETURN from a BLOCK must clean both the innermost BLOCK-owned local and the
! shadowed locals from every active scope. The innermost BLOCK object finalizes
! first, followed by its enclosing BLOCK object and the procedure object.
!
! CHECK: 3 321
! IR_CHECK: call @afs_modproc_ar43_block_return_cleanup_m_finish_guard
! IR_CHECK: call @afs_modproc_ar43_block_return_cleanup_m_finish_guard
! IR_CHECK: call @afs_modproc_ar43_block_return_cleanup_m_finish_guard
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_block_return_cleanup_m
  implicit none
  integer :: finalization_count = 0
  integer :: finalization_order = 0

  type :: cleanup_guard
    integer :: id = 0
    integer, allocatable :: payload(:)
  contains
    final :: finish_guard
  end type cleanup_guard
contains
  subroutine finish_guard(value)
    type(cleanup_guard), intent(inout) :: value

    finalization_count = finalization_count + 1
    finalization_order = finalization_order * 10 + value%id
  end subroutine finish_guard

  subroutine return_from_shadowing_block()
    type(cleanup_guard) :: guard

    guard%id = 1
    allocate(guard%payload(1))
    block
      type(cleanup_guard) :: guard

      guard%id = 2
      allocate(guard%payload(1))
      block
        type(cleanup_guard) :: guard

        guard%id = 3
        allocate(guard%payload(1))
        return
      end block
      error stop 1
    end block
    error stop 2
  end subroutine return_from_shadowing_block
end module ar43_block_return_cleanup_m

program ar43_block_return_shadow_cleanup
  use ar43_block_return_cleanup_m
  implicit none

  call return_from_shadowing_block()
  print *, finalization_count, finalization_order
  if (finalization_count /= 3) error stop 3
  if (finalization_order /= 321) error stop 4
end program ar43_block_return_shadow_cleanup
