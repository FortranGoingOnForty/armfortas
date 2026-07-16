! CHECK: ok
! REPRO_CHECK: run
program format_reversion_nested_group
  implicit none
  character(len=16) :: basic(2), suffix(2), repeated(2)
  character(len=32) :: nested(2)

  basic = ''
  write(basic, '("P",2(I0,1X))') 1, 2, 3, 4
  if (trim(basic(1)) /= 'P1 2') error stop 1
  if (trim(basic(2)) /= '3 4') error stop 2

  suffix = ''
  write(suffix, '("E",2(I0,1X),"Z",I0)') 1, 2, 3, 4, 5, 6
  if (trim(suffix(1)) /= 'E1 2 Z3') error stop 3
  if (trim(suffix(2)) /= '4 5 Z6') error stop 4

  nested = ''
  write(nested, '("B",2(I0,3("x",I0,1X)))') 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12
  if (trim(nested(1)) /= 'B1x2 x3 x4 5x6 x7 x8') error stop 5
  if (trim(nested(2)) /= '9x10 x11 x12') error stop 6

  repeated = ''
  write(repeated, '("R",2I0)') 1, 2, 3, 4
  if (trim(repeated(1)) /= 'R12') error stop 7
  if (trim(repeated(2)) /= 'R34') error stop 8

  print *, 'ok'
end program format_reversion_nested_group
