! Pins the resolution of the l04-era "PRINT with a character format
! inserts spurious spaces" noted item: print '(A,A,A)' must emit
! exactly what the format says, byte-for-byte like WRITE, including
! around short character items and function-result items whose name
! shadows an intrinsic (the historical trigger shapes).
program ltail_print_char_format_exact
  implicit none
  character(len=3) :: s
  s = 'abc'
  print '(A,A,A)', 'x[', s(1:1), ']'
  write(*,'(A,A,A)') 'y[', s(2:2), ']'
  print '(a,i0)', 'N=', scan_count()
! CHECK: x[a]
! CHECK: y[b]
! CHECK: N=3
contains
  integer function scan_count()
    scan_count = 3
  end function scan_count
end program ltail_print_char_format_exact
