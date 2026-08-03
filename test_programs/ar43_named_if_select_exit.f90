! EXIT may name non-loop constructs. Named IF and SELECT CASE exits must reach
! the construct's common end block, including through constant conditions and
! nested constructs. Exiting through an owning BLOCK must also run that
! BLOCK's finalization and deallocation before reaching the construct end.
!
! CHECK: 131071 2 30
! IR_CHECK: if_end
! IR_CHECK: select_end
! IR_CHECK: construct_exit_cleanup
! IR_CHECK: call @afs_modproc_ar43_named_if_select_exit_m_finish_guard
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_named_if_select_exit_m
  implicit none
  integer :: finalization_count = 0
  integer :: finalized_id_sum = 0

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
    finalized_id_sum = finalized_id_sum + value%id
  end subroutine finish_guard
end module ar43_named_if_select_exit_m

program ar43_named_if_select_exit
  use ar43_named_if_select_exit_m
  implicit none
  integer :: score
  integer :: selector

  score = 0
  selector = 2

  then_path: if (selector == 2) then
    score = score + 1
    exit then_path
    score = score + 1000000
  else
    score = score + 1000000
  end if then_path
  score = score + 2

  else_if_path: if (selector == 0) then
    score = score + 1000000
  else if (selector == 2) then else_if_path
    score = score + 4
    exit ELSE_IF_PATH
    score = score + 1000000
  else else_if_path
    score = score + 1000000
  end if else_if_path
  score = score + 8

  matching_case: select case (selector)
  case (1)
    score = score + 1000000
  case (2)
    score = score + 16
    exit MaTcHiNg_CaSe
    score = score + 1000000
  case default
    score = score + 1000000
  end select matching_case
  score = score + 32

  selector = 9
  default_case: select case (selector)
  case (:0)
    score = score + 1000000
  case default
    score = score + 64
    exit default_case
    score = score + 1000000
  end select default_case
  score = score + 128

  ! The constant-condition lowering path must keep the named destination
  ! active while lowering a nested SELECT CASE.
  outer_if: if (.true.) then
    nested_case: select case (1)
    case (1)
      score = score + 256
      exit OuTeR_If
      score = score + 1000000
    case default
      score = score + 1000000
    end select nested_case
    score = score + 1000000
  else
    score = score + 1000000
  end if outer_if
  score = score + 512

  ! Exiting the inner construct must not exit its enclosing SELECT CASE.
  outer_case: select case (1)
  case (1)
    inner_if: if (.true.) then
      score = score + 1024
      exit inner_if
      score = score + 1000000
    end if inner_if
    score = score + 2048
    exit outer_case
    score = score + 1000000
  case default
    score = score + 1000000
  end select outer_case
  score = score + 4096

  cleanup_if: if (selector == 9) then
    block
      type(cleanup_guard) :: guard
      guard%id = 10
      allocate(guard%payload(1))
      score = score + 8192
      exit cleanup_if
      score = score + 1000000
    end block
    score = score + 1000000
  end if cleanup_if
  if (finalization_count /= 1) error stop 1
  if (finalized_id_sum /= 10) error stop 2
  score = score + 16384

  cleanup_case: select case (selector)
  case (9)
    block
      type(cleanup_guard) :: guard
      guard%id = 20
      allocate(guard%payload(1))
      score = score + 32768
      exit cleanup_case
      score = score + 1000000
    end block
    score = score + 1000000
  case default
    score = score + 1000000
  end select cleanup_case
  if (finalization_count /= 2) error stop 3
  if (finalized_id_sum /= 30) error stop 4
  score = score + 65536

  print *, score, finalization_count, finalized_id_sum
  if (score /= 131071) error stop 5
end program ar43_named_if_select_exit
