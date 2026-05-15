! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: obj|repro
program formatted_write_g0_iostat
  implicit none
  character(len=128) :: buffer
  character(len=16) :: fmt
  integer :: stat
  integer :: fails

  fails = 0

  buffer = ""
  stat = -99
  write(buffer, "(g0)", iostat=stat) 100.0
  if (stat /= 0 .or. index(trim(buffer), "100.0") /= 1) fails = fails + 1

  buffer = ""
  stat = -99
  write(buffer, "(g0)", iostat=stat) -1026191
  if (stat /= 0 .or. trim(buffer) /= "-1026191") fails = fails + 1

  buffer = ""
  stat = -99
  write(buffer, "(g0)", iostat=stat) .true.
  if (stat /= 0 .or. trim(buffer) /= "T") fails = fails + 1

  buffer = ""
  stat = -99
  write(buffer, "(F6.2)", iostat=stat) -100.0
  if (stat /= 0 .or. trim(buffer) /= "******") fails = fails + 1

  buffer = ""
  stat = -99
  write(buffer, "(F6.3)", iostat=stat) 1000.0
  if (stat /= 0 .or. trim(buffer) /= "******") fails = fails + 1

  buffer = ""
  stat = -99
  write(buffer, "(1x)", iostat=stat) .false.
  if (stat == 0) fails = fails + 1

  buffer = ""
  stat = -99
  fmt = "(7.3)"
  write(buffer, fmt, iostat=stat) 1000.0
  if (stat == 0) fails = fails + 1

  if (fails /= 0) then
    print *, "fail"
    error stop
  end if

  print *, "ok"
end program
