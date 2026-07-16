! CHECK: ok
program statement_iostat_kinds
  implicit none

  type :: status_box
    integer(1) :: ios1
    integer(1) :: guard1
    integer(2) :: ios2
    integer(2) :: guard2
    integer(4) :: spacer
    integer(8) :: ios8
    integer(8) :: guard8
  end type status_box

  type :: unit_box
    integer(1) :: unit
    integer(1) :: guard
  end type unit_box

  type(status_box) :: status
  type(unit_box) :: opened
  character(len=32) :: message

  opened%unit = 0
  opened%guard = 37
  message = 'stale'
  call reset_status(status)
  open(newunit=opened%unit, status='scratch', action='readwrite', &
       form='formatted', iostat=status%ios1, iomsg=message)
  call check_kind1(status, 1)
  if (opened%unit >= 0 .or. opened%guard /= 37) error stop 1
  if (len_trim(message) /= 0) error stop 2

  write(opened%unit, *) 42

  message = 'stale'
  call reset_status(status)
  flush(unit=opened%unit, iostat=status%ios2, iomsg=message)
  call check_kind2(status, 2)
  if (len_trim(message) /= 0) error stop 3

  message = 'stale'
  call reset_status(status)
  rewind(unit=opened%unit, iostat=status%ios8, iomsg=message)
  call check_kind8(status, 3)
  if (len_trim(message) /= 0) error stop 4

  message = 'stale'
  call reset_status(status)
  close(unit=opened%unit, iostat=status%ios1, iomsg=message)
  call check_kind1(status, 4)
  if (len_trim(message) /= 0) error stop 5

  message = 'stale'
  call reset_status(status)
  flush(unit=123, iostat=status%ios1, iomsg=message, err=100)
  error stop 6
100 continue
  call check_kind1_error(status, 5)
  if (len_trim(message) == 0 .or. message == 'stale') error stop 7

  message = 'stale'
  call reset_status(status)
  rewind(unit=123, iostat=status%ios2, iomsg=message)
  call check_kind2_error(status, 6)
  if (len_trim(message) == 0 .or. message == 'stale') error stop 8

  print *, 'ok'

contains

  subroutine reset_status(value)
    type(status_box), intent(out) :: value

    value%ios1 = 77
    value%guard1 = 11
    value%ios2 = 777
    value%guard2 = 22
    value%spacer = 4444
    value%ios8 = 4294967296_8
    value%guard8 = 88
  end subroutine reset_status

  subroutine check_kind1(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 0 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88) then
      print *, 'kind 1 status corruption', tag
      error stop 101
    end if
  end subroutine check_kind1

  subroutine check_kind2(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 77 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 0 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88) then
      print *, 'kind 2 status corruption', tag
      error stop 102
    end if
  end subroutine check_kind2

  subroutine check_kind8(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 77 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 0 .or. &
        value%guard8 /= 88) then
      print *, 'kind 8 status corruption', tag
      error stop 108
    end if
  end subroutine check_kind8

  subroutine check_kind1_error(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 == 0 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88) then
      print *, 'kind 1 error status corruption', tag
      error stop 111
    end if
  end subroutine check_kind1_error

  subroutine check_kind2_error(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 77 .or. value%guard1 /= 11 .or. &
        value%ios2 == 0 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88) then
      print *, 'kind 2 error status corruption', tag
      error stop 112
    end if
  end subroutine check_kind2_error

end program statement_iostat_kinds
