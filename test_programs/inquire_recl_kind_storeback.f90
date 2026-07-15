! CHECK: ok
program inquire_recl_kind_storeback
  implicit none

  type :: results
    integer(1) :: recl1
    integer(1) :: guard1
    integer(2) :: recl2
    integer(2) :: guard2
    integer(4) :: recl4
    integer(4) :: guard4
    integer(8) :: recl8
    integer(8) :: guard8
  end type results

  type(results) :: value
  integer :: unit

  value%recl1 = 0
  value%guard1 = 11
  value%recl2 = 0
  value%guard2 = 22
  value%recl4 = 0
  value%guard4 = 44
  value%recl8 = 0
  value%guard8 = 88

  open(newunit=unit, status='scratch', access='stream', form='formatted')
  inquire(unit=unit, recl=value%recl1)
  inquire(unit=unit, recl=value%recl2)
  inquire(unit=unit, recl=value%recl4)
  inquire(unit=unit, recl=value%recl8)
  close(unit)

  if (value%recl1 /= -1 .or. value%guard1 /= 11) error stop 1
  if (value%recl2 /= -1 .or. value%guard2 /= 22) error stop 2
  if (value%recl4 /= -1 .or. value%guard4 /= 44) error stop 3
  if (value%recl8 /= -1 .or. value%guard8 /= 88) error stop 4

  print *, 'ok'
end program inquire_recl_kind_storeback
