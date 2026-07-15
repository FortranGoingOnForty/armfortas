! CHECK: ok
program data_transfer_iostat_kinds
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

  type(status_box) :: status
  character(len=32) :: buffer
  integer :: unit, value

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')

  call reset_status(status)
  write(unit, *, iostat=status%ios1) 11
  call check_kind1(status, 1)
  call reset_status(status)
  write(unit, *, iostat=status%ios2) 22
  call check_kind2(status, 2)
  call reset_status(status)
  write(unit, *, iostat=status%ios8) 33
  call check_kind8(status, 3)

  rewind(unit)
  call reset_status(status)
  read(unit, *, iostat=status%ios1) value
  call check_kind1(status, 4)
  if (value /= 11) error stop 4
  call reset_status(status)
  read(unit, *, iostat=status%ios2) value
  call check_kind2(status, 5)
  if (value /= 22) error stop 5
  call reset_status(status)
  read(unit, *, iostat=status%ios8) value
  call check_kind8(status, 6)
  if (value /= 33) error stop 6
  close(unit)

  call reset_status(status)
  write(buffer, *, iostat=status%ios1) 44
  call check_kind1(status, 7)
  call reset_status(status)
  write(buffer, *, iostat=status%ios2) 55
  call check_kind2(status, 8)
  call reset_status(status)
  write(buffer, *, iostat=status%ios8) 66
  call check_kind8(status, 9)

  buffer = '77'
  call reset_status(status)
  read(buffer, *, iostat=status%ios1) value
  call check_kind1(status, 10)
  if (value /= 77) error stop 10
  buffer = '88'
  call reset_status(status)
  read(buffer, *, iostat=status%ios2) value
  call check_kind2(status, 11)
  if (value /= 88) error stop 11
  buffer = '99'
  call reset_status(status)
  read(buffer, *, iostat=status%ios8) value
  call check_kind8(status, 12)
  if (value /= 99) error stop 12

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
        value%spacer /= 4444 .or. value%ios8 /= 0 .or. value%guard8 /= 88) then
      print *, 'kind 8 status corruption', tag
      error stop 108
    end if
  end subroutine check_kind8

end program data_transfer_iostat_kinds
