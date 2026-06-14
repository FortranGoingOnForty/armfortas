! l05-2: F2023 LEADING_ZERO= specifier on OPEN, WRITE, and INQUIRE.
! OPEN sets the connection-level mode; a plain (F6.3) write with no LZ
! edit descriptor honors it (SUPPRESS drops the leading zero). A WRITE
! statement LEADING_ZERO= overrides the connection for that statement
! only. INQUIRE reads the connection's current mode back ('SUPPRESS').
! The values are written to a file, re-read as text, and echoed with
! delimiters so the harness sees exact field contents. The specifiers
! are runtime-threaded, so output is opt-level invariant (OPT_EQ).
! CHECK: conn |.250|
! CHECK: stmt |0.250|
! CHECK: inq |SUPPRESS|
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x_l05_leading_zero_specifier
  implicit none
  character(len=20) :: lzmode
  character(len=32) :: line1, line2
  integer :: u

  u = 37
  open(u, file='afs_lz_spec.tmp', status='replace', leading_zero='suppress')
  write(u, '(F6.3)') 0.25d0                       ! connection: SUPPRESS -> .250
  write(u, '(F6.3)', leading_zero='print') 0.25d0 ! statement override -> 0.250
  inquire(u, leading_zero=lzmode)                 ! connection mode = SUPPRESS
  close(u)

  open(u, file='afs_lz_spec.tmp', status='old')
  read(u, '(A)') line1
  read(u, '(A)') line2
  close(u, status='delete')

  write(*, '(A,A,A)') 'conn |', trim(adjustl(line1)), '|'
  write(*, '(A,A,A)') 'stmt |', trim(adjustl(line2)), '|'
  write(*, '(A,A,A)') 'inq |', trim(lzmode), '|'
  print '(A)', 'ok'
end program x_l05_leading_zero_specifier
