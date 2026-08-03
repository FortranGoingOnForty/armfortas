! I/O branch specifiers are implicit control transfers. If their labels lie
! outside a BLOCK, every exited scope must be finalized and deallocated before
! execution reaches the label.
!
! CHECK: 7 280
! IR_CHECK: read_end_cleanup
! IR_CHECK: read_err_cleanup
! IR_CHECK: write_err_cleanup
! IR_CHECK: io_err_cleanup
! IR_CHECK: call @afs_modproc_ar43_io_branch_cleanup_m_finish_guard
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_io_branch_cleanup_m
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
end module ar43_io_branch_cleanup_m

program ar43_io_branch_cleanup
  use ar43_io_branch_cleanup_m
  implicit none
  integer :: unit_number
  integer :: value
  character(len=3) :: invalid_integer

  invalid_integer = 'bad'

  open(newunit=unit_number, status='scratch', action='readwrite')
  rewind(unit_number)
  block
    type(cleanup_guard) :: guard
    guard%id = 10
    allocate(guard%payload(1))
    read(unit_number, *, end=100) value
    error stop 1
  end block
  error stop 2
100 continue
  if (finalization_count /= 1) error stop 3
  close(unit_number)

  block
    type(cleanup_guard) :: guard
    guard%id = 20
    allocate(guard%payload(1))
    read(invalid_integer, '(I3)', err=200) value
    error stop 4
  end block
  error stop 5
200 continue
  if (finalization_count /= 2) error stop 6

  open(newunit=unit_number, status='scratch', action='read')
  block
    type(cleanup_guard) :: guard
    guard%id = 30
    allocate(guard%payload(1))
    write(unit_number, *, err=300) 1234
    error stop 7
  end block
  error stop 8
300 continue
  if (finalization_count /= 3) error stop 9
  close(unit_number)

  block
    type(cleanup_guard) :: guard
    guard%id = 40
    allocate(guard%payload(1))
    open(newunit=unit_number, file='ar43_io_cleanup_missing/no-file', &
         status='old', action='read', err=400)
    error stop 10
  end block
  error stop 11
400 continue
  if (finalization_count /= 4) error stop 12

  ! A branch to a label inside the same BLOCK must not finalize early.
  block
    type(cleanup_guard) :: guard
    guard%id = 50
    allocate(guard%payload(1))
    read(invalid_integer, '(I3)', err=510) value
    error stop 13
510 continue
    if (finalization_count /= 4) error stop 14
  end block
  if (finalization_count /= 5) error stop 15

  ! A branch from an inner BLOCK to its enclosing BLOCK cleans only the inner
  ! scope; the outer guard remains alive until the outer END BLOCK.
  block
    type(cleanup_guard) :: outer_guard
    outer_guard%id = 60
    allocate(outer_guard%payload(1))
    block
      type(cleanup_guard) :: inner_guard
      inner_guard%id = 70
      allocate(inner_guard%payload(1))
      read(invalid_integer, '(I3)', err=610) value
      error stop 16
    end block
    error stop 17
610 continue
    if (finalization_count /= 6) error stop 18
    if (finalized_id_sum /= 220) error stop 19
  end block

  print *, finalization_count, finalized_id_sum
  if (finalization_count /= 7) error stop 20
  if (finalized_id_sum /= 280) error stop 21
end program ar43_io_branch_cleanup
