! CHECK: 30 300 3000
! IR_CHECK: call @afs_fmt_end
! IR_CHECK: call @afs_fmt_read_string_noadvance
! REPRO_CHECK: run
program p
  use, intrinsic :: iso_fortran_env, only : iostat_eor
  implicit none

  integer, parameter :: bufsize = 4096
  integer :: unit_num, i, stat, chunk
  integer :: lens(3)
  character(len=bufsize) :: buffer
  character(len=:), allocatable :: line

  open(newunit=unit_num, status='scratch')
  write(unit_num, '(a)') repeat('abc', 10), repeat('def', 100), repeat('ghi', 1000)
  rewind(unit_num)

  do i = 1, 3
    line = ''
    stat = 0
    do while (stat == 0)
      chunk = -1
      read(unit_num, '(a)', advance='no', iostat=stat, size=chunk) buffer
      if (stat > 0) error stop 1
      line = line // buffer(:chunk)
    end do
    if (stat /= iostat_eor) error stop 2
    lens(i) = len(line)
  end do

  close(unit_num)
  if (lens(1) /= 30 .or. lens(2) /= 300 .or. lens(3) /= 3000) error stop 3
  print *, lens(1), lens(2), lens(3)
end program p
