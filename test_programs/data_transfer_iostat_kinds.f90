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
    integer(16) :: ios16
    integer(16) :: guard16
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
  call reset_status(status)
  write(unit, *, iostat=status%ios16) 44
  call check_kind16(status, 13)

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
  call reset_status(status)
  read(unit, *, iostat=status%ios16) value
  call check_kind16(status, 14)
  if (value /= 44) error stop 14
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
  call reset_status(status)
  write(buffer, *, iostat=status%ios16) 77
  call check_kind16(status, 15)

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
  buffer = '111'
  call reset_status(status)
  read(buffer, *, iostat=status%ios16) value
  call check_kind16(status, 16)
  if (value /= 111) error stop 16

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
    value%ios16 = 1234567890123456789_16
    value%guard16 = 1616_16
  end subroutine reset_status

  subroutine check_kind1(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 0 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88 .or. value%ios16 /= 1234567890123456789_16 .or. &
        value%guard16 /= 1616_16) then
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
        value%guard8 /= 88 .or. value%ios16 /= 1234567890123456789_16 .or. &
        value%guard16 /= 1616_16) then
      print *, 'kind 2 status corruption', tag
      error stop 102
    end if
  end subroutine check_kind2

  subroutine check_kind8(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 77 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 0 .or. value%guard8 /= 88 .or. &
        value%ios16 /= 1234567890123456789_16 .or. value%guard16 /= 1616_16) then
      print *, 'kind 8 status corruption', tag
      error stop 108
    end if
  end subroutine check_kind8

  subroutine check_kind16(value, tag)
    type(status_box), intent(in) :: value
    integer, intent(in) :: tag

    if (value%ios1 /= 77 .or. value%guard1 /= 11 .or. &
        value%ios2 /= 777 .or. value%guard2 /= 22 .or. &
        value%spacer /= 4444 .or. value%ios8 /= 4294967296_8 .or. &
        value%guard8 /= 88 .or. value%ios16 /= 0 .or. value%guard16 /= 1616_16) then
      print *, 'kind 16 status corruption', tag
      error stop 116
    end if
  end subroutine check_kind16

end program data_transfer_iostat_kinds
