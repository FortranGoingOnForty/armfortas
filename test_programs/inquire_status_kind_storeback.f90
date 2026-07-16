! CHECK: ok
program inquire_status_kind_storeback
  implicit none

  type :: status_results
    integer(1) :: value1
    integer(1) :: guard1
    integer(2) :: value2
    integer(2) :: guard2
    integer(4) :: value4
    integer(4) :: guard4
    integer(8) :: value8
    integer(8) :: guard8
    integer(16) :: value16
    integer(16) :: guard16
  end type status_results

  type :: logical_results
    logical(1) :: value1
    integer(1) :: guard1
    logical(2) :: value2
    integer(2) :: guard2
    logical(4) :: value4
    integer(4) :: guard4
    logical(8) :: value8
    integer(8) :: guard8
    logical(16) :: value16
    integer(16) :: guard16
  end type logical_results

  type(status_results) :: status
  type(logical_results) :: opened
  integer :: unit

  call reset_status(status)
  call reset_logical(opened)
  open(newunit=unit, status='scratch', action='readwrite')

  inquire(unit=unit, opened=opened%value1, iostat=status%value1)
  inquire(unit=unit, opened=opened%value2, iostat=status%value2)
  inquire(unit=unit, opened=opened%value4, iostat=status%value4)
  inquire(unit=unit, opened=opened%value8, iostat=status%value8)
  inquire(unit=unit, opened=opened%value16, iostat=status%value16)
  close(unit)

  if (status%value1 /= 0 .or. status%guard1 /= 11) error stop 1
  if (status%value2 /= 0 .or. status%guard2 /= 22) error stop 2
  if (status%value4 /= 0 .or. status%guard4 /= 44) error stop 3
  if (status%value8 /= 0 .or. status%guard8 /= 88) error stop 4
  if (status%value16 /= 0 .or. status%guard16 /= 1616) error stop 5

  if (.not. opened%value1 .or. opened%guard1 /= 11) error stop 6
  if (.not. opened%value2 .or. opened%guard2 /= 22) error stop 7
  if (.not. opened%value4 .or. opened%guard4 /= 44) error stop 8
  if (.not. opened%value8 .or. opened%guard8 /= 88) error stop 9
  if (.not. opened%value16 .or. opened%guard16 /= 1616) error stop 10

  print *, 'ok'

contains

  subroutine reset_status(value)
    type(status_results), intent(out) :: value

    value%value1 = 77
    value%guard1 = 11
    value%value2 = 777
    value%guard2 = 22
    value%value4 = 7777
    value%guard4 = 44
    value%value8 = 77777777_8
    value%guard8 = 88
    value%value16 = 77777777_16
    value%guard16 = 1616
  end subroutine reset_status

  subroutine reset_logical(value)
    type(logical_results), intent(out) :: value

    value%value1 = .false.
    value%guard1 = 11
    value%value2 = .false.
    value%guard2 = 22
    value%value4 = .false.
    value%guard4 = 44
    value%value8 = .false.
    value%guard8 = 88
    value%value16 = .false.
    value%guard16 = 1616
  end subroutine reset_logical

end program inquire_status_kind_storeback
