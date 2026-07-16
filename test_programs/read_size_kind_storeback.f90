! CHECK: ok
program read_size_kind_storeback
  use, intrinsic :: iso_fortran_env, only: iostat_eor
  implicit none

  type :: size_results
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
  end type size_results

  type(size_results) :: sizes
  character(len=2) :: text
  character(len=4) :: first_part, second_part
  integer :: unit, ios, count

  call reset_sizes(sizes)
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'abcdef'

  rewind(unit)
  read(unit, '(A2)', advance='no', size=sizes%value1, iostat=ios) text
  rewind(unit)
  read(unit, '(A2)', advance='no', size=sizes%value2, iostat=ios) text
  rewind(unit)
  read(unit, '(A2)', advance='no', size=sizes%value4, iostat=ios) text
  rewind(unit)
  read(unit, '(A2)', advance='no', size=sizes%value8, iostat=ios) text
  rewind(unit)
  read(unit, '(A2)', advance='no', size=sizes%value16, iostat=ios) text

  rewind(unit)
  count = -1
  first_part = ''
  second_part = ''
  read(unit, '(A2,A2)', advance='no', size=count, iostat=ios) first_part, second_part
  if (ios /= 0 .or. count /= 4) error stop 6
  if (first_part /= 'ab' .or. second_part /= 'cd') error stop 7

  rewind(unit)
  count = -1
  first_part = ''
  second_part = ''
  read(unit, '(A4,A4)', advance='no', size=count, iostat=ios) first_part, second_part
  if (ios /= iostat_eor .or. count /= 6) error stop 8
  if (first_part /= 'abcd' .or. second_part /= 'ef') error stop 9
  close(unit)

  if (sizes%value1 /= 2 .or. sizes%guard1 /= 11) error stop 1
  if (sizes%value2 /= 2 .or. sizes%guard2 /= 22) error stop 2
  if (sizes%value4 /= 2 .or. sizes%guard4 /= 44) error stop 3
  if (sizes%value8 /= 2 .or. sizes%guard8 /= 88) error stop 4
  if (sizes%value16 /= 2 .or. sizes%guard16 /= 1616) error stop 5

  print *, 'ok'

contains

  subroutine reset_sizes(value)
    type(size_results), intent(out) :: value

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
  end subroutine reset_sizes

end program read_size_kind_storeback
